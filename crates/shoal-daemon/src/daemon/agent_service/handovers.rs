//! The hub, through which a shutdown, or the user walking away or pausing
//! the machine, hands this machine's spaces over.
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Handovers;
use shoal_protocol::hub::packages::{Handed, Handing, Leaving};
use tokio::time::Instant;

use crate::{daemon::manager::Manager, model::Space};

impl Handovers for Manager {
    fn hands_over(&self, leaving: Leaving) -> BoxFuture<'_, bool> {
        Box::pin(self.hub.hands_over(self, leaving))
    }

    fn hand_over(
        &self,
        deadline: Instant,
        handing: Handing,
        leaving: Leaving,
    ) -> BoxFuture<'_, Vec<(Space, Handed)>> {
        Box::pin(async move {
            // A CI job's workspace belongs to this machine's runner.
            let jobs = self.runner.job_workspaces().await;
            let handing = (&handing, &jobs);
            let handed = self.hub.hand_over(self, deadline, handing, leaving).await;
            handed.unwrap_or_default()
        })
    }

    fn stay(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.hub.stay(self))
    }
}
