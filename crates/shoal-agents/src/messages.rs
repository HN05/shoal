//! Alerts for the agent of a space, a workspace or a chat, from the user,
//! Shoal or the spaces above and below it. The daemon never writes to an
//! agent's terminal: agents receive alerts through `shoal alert list`,
//! watches and agent hooks, which mark them read. Files attached to an
//! alert are the daemon's copies among its space's attached files, which
//! outlive it until the space is removed.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;
use rusqlite::Transaction;

use crate::{
    Agents,
    daemon::{Daemon, Records},
};
use shoal_daemon_base::alerts::{About, AlertKind, NewAlert, Unread};
use shoal_protocol::space_names::SpaceSelector;

mod settle;
mod store;

pub use settle::SettleClock;
pub use store::{ensure_holders_fit, every_unread, hand_to_user};

/// How a queued alert reaches an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// It starts the agent's next turn.
    Wake,
    /// It waits for the agent's next turn or update.
    NextTurn,
}

/// An alert queued for a space's agent: its ID, and whether queueing it
/// changed what the agent receives rather than finding it unread already.
#[derive(Debug, Clone, Copy)]
pub struct Queued {
    pub id: i64,
    pub new: bool,
}

/// Records the copies of the files an alert attaches among the attached
/// files of the space it goes to, by its ID, in the transaction that
/// queues it.
pub type RecordCopies = dyn Fn(&Transaction<'_>, &SpaceId) -> Result<()> + Send;

/// Queue Shoal's alert of `kind`, about no other space, for the agent of
/// the space `id` in the caller's transaction, as
/// [`Agents::queue_alert`] queues one, for a caller that decides in the
/// same transaction whether to send it: its ID, or `None` when an identical
/// alert is unread. The caller wakes the waits for alerts after committing.
pub fn queue_shoal_message(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    kind: AlertKind,
    message: &str,
) -> Result<Option<i64>> {
    let alert = NewAlert::shoal(kind, About::default(), message.to_owned());
    let queued = queue_in(tx, records, id, &alert)?;
    Ok(queued.new.then_some(queued.id))
}

/// Queue `alert`, whose files another space's files already keep, for the
/// agent of the space `id` in the caller's transaction, waking an idle
/// agent, as [`Agents::queue_alert`] queues one. The caller wakes the
/// waits for alerts after committing.
pub fn queue_in(
    tx: &Transaction<'_>,
    records: &dyn Records,
    id: &SpaceId,
    alert: &NewAlert,
) -> Result<Queued> {
    store::insert(tx, records, id, alert, Delivery::Wake, false, None)
}

impl Agents {
    /// Queue `alert`, whose files the daemon copied into the space's
    /// attached files, which `record` records in the transaction that
    /// queues it, for the agent of the space `id`. Returns the alert's ID.
    pub async fn queue_copied(
        &self,
        id: &SpaceId,
        alert: NewAlert,
        record: Box<RecordCopies>,
    ) -> Result<i64> {
        self.queue(id, alert, false, Delivery::Wake, Some(record))
            .await
    }

    /// Queue `alert` for the agent of the space `id`, whose files another
    /// space's files already keep; `for_user` says the user receives it if
    /// the space goes before its agent reads it. A space whose agent is not
    /// running receives it once its agent starts. It reads the space as
    /// recorded, stopping no chat, so a caller holding the approval
    /// announcement gate may queue one. Returns the alert's ID.
    pub async fn queue_alert(&self, id: &SpaceId, alert: NewAlert, for_user: bool) -> Result<i64> {
        self.queue(id, alert, for_user, Delivery::Wake, None).await
    }

    /// Queue Shoal's alert of `kind`, about no other space, for the agent
    /// of the space `id`.
    pub async fn queue_agent_message(
        &self,
        id: &SpaceId,
        kind: AlertKind,
        message: String,
    ) -> Result<()> {
        (self.queue_agent_message_delivered(id, kind, message, Delivery::Wake)).await
    }

    pub async fn queue_agent_message_delivered(
        &self,
        id: &SpaceId,
        kind: AlertKind,
        message: String,
        delivery: Delivery,
    ) -> Result<()> {
        let alert = NewAlert::shoal(kind, About::default(), message);
        self.queue(id, alert, false, delivery, None).await?;
        Ok(())
    }

    /// Queue `alert` for the agent of the space `id` with the files it
    /// `copied`, then wake the waits for it. Returns the alert's ID.
    async fn queue(
        &self,
        id: &SpaceId,
        alert: NewAlert,
        for_user: bool,
        delivery: Delivery,
        copied: Option<Box<RecordCopies>>,
    ) -> Result<i64> {
        let (id, records) = (id.to_owned(), self.records.clone());
        let queued = self
            .store
            .write(move |tx| {
                let copied = copied.as_deref();
                store::insert(tx, &*records, &id, &alert, delivery, for_user, copied)
            })
            .await?;
        if queued.new {
            self.signals.message_queued();
        }
        Ok(queued.id)
    }

    /// The unread messages of the space `selector` names, oldest first.
    /// They stay unread until delivered.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn agent_messages(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Vec<shoal_daemon_base::alerts::Alert>> {
        let space = daemon.space(selector).await?;
        Ok(self.unread_messages(&space.id).await?.alerts)
    }

    /// The unread messages of the space `id`, oldest first, and whether one
    /// of them wakes an idle agent.
    pub async fn unread_messages(&self, id: &SpaceId) -> Result<Unread> {
        let id = id.to_owned();
        self.store.read(move |db| store::unread(db, &id)).await
    }

    /// Mark exactly these alerts read once the agent of the space
    /// `selector` names has received them.
    pub async fn mark_agent_messages_delivered(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        ids: Vec<i64>,
    ) -> Result<()> {
        let space = daemon.space(selector).await?.id.clone();
        self.store
            .write(move |tx| {
                for id in ids {
                    store::mark_delivered(tx, &space, id)?;
                }
                Ok(())
            })
            .await?;
        self.signals.agent_changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
