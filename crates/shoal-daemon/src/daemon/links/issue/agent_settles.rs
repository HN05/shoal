//! Settling the agent account on the issues of the spaces whose agent
//! started or stopped, or whose work was done or resumed, in the
//! background, so a slow forge holds none of those back.
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

use tokio::sync::Notify;

use crate::daemon::manager::Manager;

/// The spaces whose issues wait for the agent account to be settled.
#[derive(Default)]
pub(crate) struct AgentSettles {
    requested: Mutex<HashSet<SpaceId>>,
    wake: Notify,
}

impl AgentSettles {
    /// Settle the agent account on the issues of the space `id`.
    pub(crate) fn request(&self, id: &SpaceId) {
        self.lock().insert(id.clone());
        self.wake.notify_one();
    }

    fn take(&self) -> Vec<SpaceId> {
        self.lock().drain().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<SpaceId>> {
        self.requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Settle each requested space as it is requested.
pub async fn run(manager: Arc<Manager>) {
    loop {
        manager.settle_requested_agents().await;
        manager.agent_settles.wake.notified().await;
    }
}

impl Manager {
    /// Settle the agent account on the issues of each space requested
    /// since the last time.
    pub(crate) async fn settle_requested_agents(&self) {
        for id in self.agent_settles.take() {
            self.settle_agent_assignees(&id).await;
        }
    }
}
