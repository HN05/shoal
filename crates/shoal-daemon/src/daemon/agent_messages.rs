//! Messages for the agent of a space, a workspace or a chat, from the user,
//! the daemon or the spaces below it. The daemon never writes to an agent's
//! terminal: agents receive messages through `shoal messages`, watches and
//! agent hooks, and a delivered message is deleted. Files attached to a
//! message outlive it, until their space is removed (see
//! [`crate::daemon::workspace_files`]).
use crate::client::space_ids::SpaceId;
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::Transaction;

pub(crate) use crate::client::agent_messages::AgentMessage;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    daemon::{
        notifications::NotificationKind,
        workspace::Manager,
        workspace_files::{self, Attacher, NewFile, layout::Copy},
    },
    model::PresentSpace,
};

mod recipients;
mod settle;
mod store;

pub(crate) use settle::SettleClock;
pub(crate) use store::{every_unread, hand_to_user, rebase_attachments};

/// Unread messages one space may hold, and their total size, attached
/// paths included; together they fit one response.
pub(crate) const MAX_UNREAD: i64 = 50;
pub(crate) const MAX_UNREAD_BYTES: usize = 50 * crate::validate::MAX_MESSAGE_LEN;

/// How a queued message reaches an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// It starts the agent's next turn.
    Wake,
    /// It waits for the agent's next turn or update.
    NextTurn,
}

/// The space a message for another space's agent is about.
#[derive(Debug, Clone)]
pub(crate) struct About {
    pub name: SpaceName,
    pub id: Option<SpaceId>,
    /// The notification the event would be for the user, which the user
    /// receives instead if the space goes before its agent reads it.
    pub kind: Option<NotificationKind>,
}

/// A space's unread messages.
#[derive(Debug, Default)]
pub(crate) struct Unread {
    pub messages: Vec<AgentMessage>,
    /// Whether one of them starts an idle agent's next turn.
    pub wakes: bool,
}

/// The files a queued message attaches.
enum Attached {
    /// Shoal's copies of files another space's files already keep.
    Kept(Vec<PathBuf>),
    /// Copies the recipient's attached files list from then on as attached
    /// by the attacher.
    Copied(Vec<Copy>, Attacher),
}

impl Attached {
    /// The paths of Shoal's copies.
    fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Kept(paths) => paths.clone(),
            Self::Copied(copies, _) => copies.iter().map(|copy| copy.path.clone()).collect(),
        }
    }

    /// Record the copies made for a message to the space `id` among its
    /// attached files, in the transaction that queues it.
    fn record(&self, tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
        match self {
            Self::Kept(_) => Ok(()),
            Self::Copied(copies, attacher) => {
                let source = workspace_files::FileSource::Message;
                workspace_files::record(tx, id, source, Some(attacher), copies)
            }
        }
    }
}

/// Queue `message` from Shoal, about no other space, for the agent of the
/// space `id` in the caller's transaction, as
/// [`Manager::queue_space_message`] queues one, for a caller that decides
/// in the same transaction whether to send it: its ID, or `None` when an
/// identical message is unread. The caller wakes the space's waits with
/// [`Manager::space_message_queued`] after committing.
pub(crate) fn queue_shoal_message(
    tx: &Transaction<'_>,
    id: &SpaceId,
    message: &str,
) -> Result<Option<i64>> {
    store::insert(tx, id, None, message, &[], Delivery::Wake)
}

impl Manager {
    /// Queue a message for the agent of `space`, with copies of the files
    /// `attachments`, attached by `attacher`; without text, the files are
    /// the message. An identical unread message is not repeated.
    async fn send_to(
        &self,
        space: PresentSpace,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let id = space.id().clone();
        if attachments.is_empty() {
            return self.queue_agent_message(&id, message).await;
        }
        let text = message.clone();
        self.queue_attached(space.into(), &text, attachments, |copies| {
            let attached = Attached::Copied(copies, attacher.clone());
            self.queue(&id, None, message, attached, Delivery::Wake)
        })
        .await
    }

    /// Queue `message` for the agent of the space `id`, with Shoal's copies
    /// `attachments` of files another space's files already keep, `about`
    /// another space when it concerns one. A space whose agent is not
    /// running receives it once its agent starts. It reads the space as
    /// recorded, stopping no chat, so a caller holding the approval
    /// announcement gate may queue one.
    pub(crate) async fn queue_space_message(
        &self,
        id: &SpaceId,
        about: Option<About>,
        message: String,
        attachments: Vec<PathBuf>,
    ) -> Result<()> {
        let attached = Attached::Kept(attachments);
        self.queue(id, about, message, attached, Delivery::Wake)
            .await
    }

    pub(crate) async fn queue_agent_message(&self, id: &SpaceId, message: String) -> Result<()> {
        self.queue_agent_message_delivered(id, message, Delivery::Wake)
            .await
    }

    pub(crate) async fn queue_agent_message_delivered(
        &self,
        id: &SpaceId,
        message: String,
        delivery: Delivery,
    ) -> Result<()> {
        let attached = Attached::Kept(Vec::new());
        self.queue(id, None, message, attached, delivery).await
    }

    /// Queue `message` for the agent of the space `id` with the files
    /// `attached`, `about` another space when it concerns one, then wake
    /// the waits for it.
    async fn queue(
        &self,
        id: &SpaceId,
        about: Option<About>,
        message: String,
        attached: Attached,
        delivery: Delivery,
    ) -> Result<()> {
        let id = id.to_owned();
        let queued = self
            .store
            .write(move |tx| {
                let paths = attached.paths();
                let queued = store::insert(tx, &id, about.as_ref(), &message, &paths, delivery)?;
                attached.record(tx, &id)?;
                Ok(queued.is_some())
            })
            .await?;
        if queued {
            self.space_message_queued();
        }
        Ok(())
    }

    /// Wake the waits for spaces' messages, after the transaction that
    /// queued one commits.
    pub(crate) fn space_message_queued(&self) {
        self.signals.agent_message_queued();
        self.signals.chat_message_queued();
    }

    /// The unread messages of the space `selector` names, oldest first.
    /// They stay unread until delivered.
    pub async fn agent_messages(&self, selector: &SpaceSelector) -> Result<Vec<AgentMessage>> {
        Ok(self.unread_agent_messages(selector).await?.messages)
    }

    /// The unread messages of the space `selector` names, oldest first, and
    /// whether one of them wakes an idle agent.
    pub(crate) async fn unread_agent_messages(&self, selector: &SpaceSelector) -> Result<Unread> {
        let id = self.space(selector).await?.id().clone();
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

    /// Delete exactly these messages once the agent of the space `selector`
    /// names has received them, with the questions whose answers they
    /// carried.
    pub async fn mark_agent_messages_delivered(
        &self,
        selector: &SpaceSelector,
        ids: Vec<i64>,
    ) -> Result<()> {
        let space = self.space(selector).await?.id().clone();
        self.store
            .write(move |tx| {
                for id in ids {
                    store::delete_delivered(tx, &space, id)?;
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
