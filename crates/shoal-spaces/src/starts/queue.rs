//! Queuing a new space's agent: the space takes the queue entry of the
//! admitted launch a start carries out, or its agent queues through the
//! queue's one request.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

use super::{Preparing, home::Recorded};
use crate::{
    log,
    model::{QueueAdmission, Space},
};

impl Spaces {
    /// Links its PR as it starts once its blockers are done.
    pub(super) async fn queue_space_agent(
        &self,
        daemon: &dyn Daemon,
        recorded: &Recorded,
        preparing: &mut Preparing,
    ) -> Result<Option<QueueAdmission>> {
        // The entry is the space's before its setup can fail, so a waiting
        // launch frees its slot even once the caller is gone.
        if let Some(entry) = &preparing.queue_entry {
            self.give_start_entry(daemon, entry, &recorded.space).await;
        }
        let Some(agent) = preparing.agent.take() else {
            return Ok(None);
        };
        let place = (&recorded.space, recorded.awaits_preparation);
        let follows = (preparing.pr.clone(), preparing.blockers.clone());
        Ok(Some(
            daemon.queue_start_agent(place, *agent, follows).await?,
        ))
    }

    /// A failure, such as an entry that no longer starts, leaves the start to
    /// go on without it.
    async fn give_start_entry(&self, daemon: &dyn Daemon, entry: &QueueEntryId, space: &Space) {
        let attached = (daemon.queue_attach(entry, &SpaceSelector::id(&space.id))).await;
        if let Err(error) = attached {
            log!("queue entry {entry} not given {}: {error:#}", space.name);
        }
    }
}
