//! Alerts for the agent of a space, a workspace or a chat, from the user,
//! Shoal or the spaces above and below it. The daemon never writes to an
//! agent's terminal: agents receive alerts through `shoal alert list`,
//! watches and agent hooks, which mark them read. Files attached to an
//! alert outlive it, until their space is removed (see
//! [`crate::daemon::workspace_files`]).
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;
use rusqlite::Transaction;

use crate::{
    daemon::{
        alerts::{About, Alert, AlertKind, NewAlert, Sender},
        manager::Manager,
        workspace_files::{self, Attacher, NewFile, layout::Copy},
    },
    model::Space,
};
use shoal_protocol::space_names::SpaceSelector;

mod settle;
mod store;

pub(crate) use settle::SettleClock;
pub(crate) use store::{every_unread, hand_to_user};

/// Unread alerts one space may hold, and the bytes they may take as its
/// agent receives them, which leaves room in one response for the item
/// activity and leftovers delivered with them.
pub(crate) const MAX_UNREAD: i64 = 50;
pub(crate) const MAX_UNREAD_BYTES: usize = 40 * 1024;

/// How a queued alert reaches an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// It starts the agent's next turn.
    Wake,
    /// It waits for the agent's next turn or update.
    NextTurn,
}

/// An alert queued for a space's agent: its ID, and whether queueing it
/// changed what the agent receives rather than finding it unread already.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Queued {
    pub id: i64,
    pub new: bool,
}

/// A space's unread alerts.
#[derive(Debug, Default)]
pub(crate) struct Unread {
    pub alerts: Vec<Alert>,
    /// Whether one of them starts an idle agent's next turn.
    pub wakes: bool,
}

/// The copies a sent alert attaches, which the recipient's attached files
/// list from then on as attached by the attacher.
struct Copied(Vec<Copy>, Attacher);

impl Copied {
    /// Record the copies made for an alert to the space `id` among its
    /// attached files, in the transaction that queues it.
    fn record(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        let source = workspace_files::FileSource::Message;
        workspace_files::record(tx, id, source, &self.1, &self.0)
    }
}

/// Queue Shoal's alert of `kind`, about no other space, for the agent of
/// the space `id` in the caller's transaction, as
/// [`Manager::queue_alert`] queues one, for a caller that decides in the
/// same transaction whether to send it: its ID, or `None` when an identical
/// alert is unread. The caller wakes the waits for alerts after committing.
pub(crate) fn queue_shoal_message(
    tx: &Transaction<'_>,
    id: &SpaceId,
    kind: AlertKind,
    message: &str,
) -> Result<Option<i64>> {
    let alert = NewAlert::shoal(kind, About::default(), message.to_owned());
    let queued = store::insert(tx, id, &alert, Delivery::Wake, false, None)?;
    Ok(queued.new.then_some(queued.id))
}

impl Manager {
    /// Queue `message` from `sender` for the agent of `space`, with copies
    /// of the files `attachments`, attached by `attacher`; without text,
    /// the files are the message. An identical unread alert is not
    /// repeated. Returns the alert's ID.
    pub(super) async fn send_to(
        &self,
        space: Space,
        sender: &(Sender, About),
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<i64> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let id = space.id.clone();
        let alert = NewAlert {
            kind: AlertKind::Message,
            sender: sender.0,
            about: sender.1.clone(),
            message,
            attachments: Vec::new(),
        };
        if attachments.is_empty() {
            return self.queue(&id, alert, false, Delivery::Wake, None).await;
        }
        let text = alert.message.clone();
        self.queue_attached(space.into(), &text, attachments, |copies| {
            let alert = NewAlert {
                attachments: copies.iter().map(|copy| copy.path.clone()).collect(),
                ..alert
            };
            let copied = Copied(copies, attacher.clone());
            self.queue(&id, alert, false, Delivery::Wake, Some(copied))
        })
        .await
    }

    /// Queue `alert` for the agent of the space `id`, whose files another
    /// space's files already keep; `for_user` says the user receives it if
    /// the space goes before its agent reads it. A space whose agent is not
    /// running receives it once its agent starts. It reads the space as
    /// recorded, stopping no chat, so a caller holding the approval
    /// announcement gate may queue one. Returns the alert's ID.
    pub(crate) async fn queue_alert(
        &self,
        id: &SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> Result<i64> {
        self.queue(id, alert, for_user, Delivery::Wake, None).await
    }

    /// Queue Shoal's alert of `kind`, about no other space, for the agent
    /// of the space `id`.
    pub(crate) async fn queue_agent_message(
        &self,
        id: &SpaceId,
        kind: AlertKind,
        message: String,
    ) -> Result<()> {
        (self.queue_agent_message_delivered(id, kind, message, Delivery::Wake)).await
    }

    pub(crate) async fn queue_agent_message_delivered(
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
        copied: Option<Copied>,
    ) -> Result<i64> {
        let id = id.to_owned();
        let queued = self
            .store
            .write(move |tx| store::insert(tx, &id, &alert, delivery, for_user, copied.as_ref()))
            .await?;
        if queued.new {
            self.signals.message_queued();
        }
        Ok(queued.id)
    }

    /// The unread messages of the space `selector` names, oldest first.
    /// They stay unread until delivered.
    #[cfg(test)]
    pub async fn agent_messages(&self, selector: &SpaceSelector) -> Result<Vec<Alert>> {
        let space = self.space(selector).await?;
        Ok(self.unread_messages(&space.id).await?.alerts)
    }

    /// The unread messages of the space `id`, oldest first, and whether one
    /// of them wakes an idle agent.
    pub(crate) async fn unread_messages(&self, id: &SpaceId) -> Result<Unread> {
        let id = id.to_owned();
        self.store.read(move |db| store::unread(db, &id)).await
    }

    /// Acknowledge the item updates, messages and leftovers the agent of the
    /// space `selector` names received.
    pub async fn acknowledge_space_delivery(
        &self,
        selector: &SpaceSelector,
        deliveries: Vec<String>,
        messages: Vec<i64>,
        leftovers: &[u32],
    ) -> Result<()> {
        // Item updates first: a failure then leaves both to be delivered
        // again, while deleted messages could not be.
        self.acknowledge_pr_updates(selector, deliveries).await?;
        self.mark_agent_messages_delivered(selector, messages)
            .await?;
        self.acknowledge_leftovers(selector, leftovers).await
    }

    /// Mark exactly these alerts read once the agent of the space
    /// `selector` names has received them, and delete the questions whose
    /// answers they carried.
    pub async fn mark_agent_messages_delivered(
        &self,
        selector: &SpaceSelector,
        ids: Vec<i64>,
    ) -> Result<()> {
        let space = self.space(selector).await?.id.clone();
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
