//! Settling the agent account on a space's issues in the background, so
//! that a slow forge holds back no agent's start or stop.
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

use shoal_ids::space_ids::SpaceId;
use tokio::sync::Notify;

use crate::{Links, daemon::Daemon};

/// The spaces whose issues wait for the agent account to be settled.
#[derive(Default)]
pub struct AgentSettles {
    requested: Mutex<HashSet<SpaceId>>,
    wake: Notify,
}

impl AgentSettles {
    /// Settle the agent account on the issues of the space `id`.
    pub fn request(&self, id: &SpaceId) {
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
pub async fn run(links: Arc<Links>, daemon: Arc<dyn Daemon>) {
    loop {
        links.settle_requested_agents(&*daemon).await;
        links.agent_settles.wake.notified().await;
    }
}

impl Links {
    pub fn agent_settles(&self) -> &AgentSettles {
        &self.agent_settles
    }

    /// Settle the agent account on the issues of each space requested
    /// since the last time.
    pub async fn settle_requested_agents(&self, daemon: &dyn Daemon) {
        for id in self.agent_settles.take() {
            self.settle_agent_assignees(daemon, &id).await;
        }
    }
}
