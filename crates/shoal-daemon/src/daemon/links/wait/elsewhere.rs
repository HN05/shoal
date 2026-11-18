//! The item activity of this machine's spaces whose coordinator runs on
//! another machine: the spaces without an agent below a space elsewhere,
//! whose activity no agent here receives. It goes up through the hub as
//! the other events those spaces pass up do, waiting in the outbox while
//! the link is down, and leaves their cursors once it has gone.
use anyhow::Result;

use super::activity::acknowledge_deliveries;
use crate::{
    daemon::{hub::Event, log, workspace::Manager},
    forge::updates::Update,
    model::SpaceRef,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Pass up the updates queued for this machine's spaces whose activity
    /// a coordinator on another machine receives, while one above them
    /// there has an agent.
    pub(crate) async fn pass_up_delegated_activity(&self) -> Result<()> {
        let agents = self.tracked_agents().await;
        let groups = (self.store)
            .run(move |db| crate::daemon::workspace::delegated_elsewhere(db, &agents))
            .await?;
        for (above, spaces) in groups {
            self.pass_up_group(&above, &spaces).await;
        }
        Ok(())
    }

    /// Pass up to the space `above`, on another machine, the updates queued
    /// for `spaces`, the first of them the one directly below it, once a
    /// space above them there has an agent to receive them.
    async fn pass_up_group(&self, above: &SpaceId, spaces: &[SpaceRef]) {
        let mut coordinated = Coordinated {
            below: &spaces[0].id,
            known: None,
        };
        for space in spaces {
            let passed = self.pass_up_space(above, space, &mut coordinated).await;
            if let Err(error) = passed {
                log!("activity of {} for its coordinator: {error:#}", space.name);
            }
        }
    }

    /// Pass up to the space `above` the updates queued for `space`, unless
    /// cleanup or an item change holds its PR gate, as a delivery here
    /// waits for, or no space above there has an agent.
    async fn pass_up_space(
        &self,
        above: &SpaceId,
        space: &SpaceRef,
        coordinated: &mut Coordinated<'_>,
    ) -> Result<()> {
        let Some(_gate) = self.gates.try_lock_prs([&space.id]) else {
            return Ok(());
        };
        let updates = self.activity_to_pass_up(space).await?;
        if updates.is_empty() || !self.coordinated(coordinated).await {
            return Ok(());
        }
        self.pass_up_activity(above, space, updates).await
    }

    /// Whether a space above `coordinated.below` on another machine has an
    /// agent, asking the hub once per sweep.
    async fn coordinated(&self, coordinated: &mut Coordinated<'_>) -> bool {
        match coordinated.known {
            Some(known) => known,
            None => {
                *(coordinated.known).insert(self.coordinator_elsewhere(coordinated.below).await)
            }
        }
    }

    /// The updates queued for `space` that fit one event, with the
    /// summaries of failed checks its coordinator reads. The caller holds
    /// the space's PR gate.
    async fn activity_to_pass_up(&self, space: &SpaceRef) -> Result<Vec<Update>> {
        let batch = (self.delegated_pending(std::slice::from_ref(space), None)).await?;
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let mut updates = self.fit_response(batch, 0).await?;
        (self.summarize_failed_checks(&SpaceSelector::id(&space.id), &mut updates)).await;
        Ok(updates)
    }

    /// Pass `updates` of `space` up to the space `above`, then take them off
    /// the space's cursor.
    async fn pass_up_activity(
        &self,
        above: &SpaceId,
        space: &SpaceRef,
        updates: Vec<Update>,
    ) -> Result<()> {
        let deliveries = updates.iter().map(|update| update.delivery.clone());
        let deliveries = deliveries.collect::<Vec<_>>();
        let event = Event {
            name: &space.name,
            space: &space.id,
            kind: None,
            message: activity_message(updates),
            attachments: &[],
        };
        self.pass_up(above.clone(), event).await?;
        let id = space.id.clone();
        (self.store)
            .write(move |tx| acknowledge_deliveries(tx, &[id], &deliveries))
            .await
    }
}

/// Whether a space above `below`, the space of a group directly below one
/// on another machine, has an agent, once the hub was asked.
struct Coordinated<'a> {
    below: &'a SpaceId,
    known: Option<bool>,
}

/// The message that carries `updates` to a coordinator, one per line,
/// without the space each names, which the message is about.
fn activity_message(updates: Vec<Update>) -> String {
    let lines = updates.into_iter().map(|mut update| {
        update.workspace = None;
        update.to_string()
    });
    lines.collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests;
