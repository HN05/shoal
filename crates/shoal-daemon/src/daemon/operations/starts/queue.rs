//! Queuing a new space's agent: the space takes the queue entry of the
//! admitted launch a start carries out, or its agent queues through the
//! queue's one request, waiting for the preparation its space awaits.
use anyhow::Result;
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

use super::{Preparing, home::Recorded};
use crate::{
    daemon::{log, workspace::Manager},
    model::{QueueAdmission, Space},
};

impl Manager {
    /// Give the `recorded` space the start's queue entry and queue the
    /// agent `preparing` names there, linking its PR as it starts once
    /// its blockers are done. The agent an admitted launch starts in a
    /// reopened workspace starts on a branch brought up to date.
    pub(super) async fn queue_space_agent(
        &self,
        recorded: &Recorded,
        preparing: &mut Preparing,
    ) -> Result<Option<QueueAdmission>> {
        // The entry is the space's before its setup can fail, so a waiting
        // launch frees its slot even once the caller is gone.
        if let Some(entry) = &preparing.queue_entry {
            self.give_start_entry(entry, &recorded.space).await;
            if let Ok(workspace) = recorded.space.clone().into_workspace()
                && recorded.reused
            {
                self.refresh_admitted(&workspace).await;
            }
        }
        let Some(agent) = preparing.agent.take() else {
            return Ok(None);
        };
        let place = (&recorded.space, recorded.awaits_preparation);
        let follows = (preparing.pr.clone(), preparing.blockers.clone());
        Ok(Some(self.queue_start_agent(place, *agent, follows).await?))
    }

    /// Give the start's queue `entry` the `space` it recorded. A failure,
    /// such as an entry that no longer starts, leaves the start to go on
    /// without it.
    async fn give_start_entry(&self, entry: &QueueEntryId, space: &Space) {
        let attached = (self.queue_attach(entry, &SpaceSelector::id(&space.id))).await;
        if let Err(error) = attached {
            log!("queue entry {entry} not given {}: {error:#}", space.name);
        }
    }
}
