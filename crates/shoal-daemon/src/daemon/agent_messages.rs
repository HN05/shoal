//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted. Files attached to a message outlive it, until their
//! workspace is removed (see [`crate::daemon::workspace_files`]).
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};

pub(crate) use crate::client::agent_messages::AgentMessage;
use crate::client::space_names::SpaceSelector;
use crate::{
    client::output::messages::{Reached, ReachedSpaces, Recipient, Refused, RefusedSpace},
    daemon::{
        workspace::Manager,
        workspace_files::{self, Attacher, NewFile, layout::Copy},
    },
    model::{Chat, PresentSpace, Workspace},
    repository::Selector,
    state::WorkspaceState,
};

mod settle;

pub(crate) use settle::SettleClock;

/// Unread messages one workspace may hold, and their total size, attached
/// paths included; together they fit one response.
pub(crate) const MAX_UNREAD: i64 = 50;
pub(crate) const MAX_UNREAD_BYTES: usize = 50 * crate::validate::MAX_MESSAGE_LEN;

/// Queue `message` with the files `attachments`, which Shoal has copied, in
/// the caller's transaction, returning its ID, or `None` when an identical
/// message is still unread. Callers wake watches with
/// `agent_messages_changed` after committing. A workspace being removed
/// takes none, since removal decides from the messages it has.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    workspace_id: &SpaceId,
    message: &str,
    attachments: &[PathBuf],
) -> Result<Option<i64>> {
    insert_delivered(tx, workspace_id, message, attachments, Delivery::Wake)
}

/// How a queued message reaches an idle agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// It starts the agent's next turn.
    Wake,
    /// It waits for the agent's next turn or update.
    NextTurn,
}

/// [`insert`] with `delivery`. An identical unread message that does not
/// wake the agent wakes it once queued again with [`Delivery::Wake`].
pub(crate) fn insert_delivered(
    tx: &Transaction<'_>,
    workspace_id: &SpaceId,
    message: &str,
    attachments: &[PathBuf],
    delivery: Delivery,
) -> Result<Option<i64>> {
    let attachments = serde_json::to_string(attachments)?;
    let removing: Option<bool> = tx
        .query_row(
            "SELECT state=?2 FROM workspaces WHERE id=?1",
            params![workspace_id, WorkspaceState::Removing],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(removing != Some(true), "the workspace is being removed");
    let wakes = delivery == Delivery::Wake;
    let pending: Option<(i64, bool)> = tx
        .query_row(
            "SELECT id,wakes FROM agent_messages
                WHERE workspace_id=?1 AND message=?2 AND attachments=?3",
            params![workspace_id, message, attachments],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, woke)) = pending {
        if woke || !wakes {
            return Ok(None);
        }
        tx.execute("UPDATE agent_messages SET wakes=1 WHERE id=?1", [id])?;
        return Ok(Some(id));
    }
    let (unread, bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(LENGTH(CAST(message||attachments AS BLOB))),0)
            FROM agent_messages WHERE workspace_id=?1",
        [workspace_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        unread < MAX_UNREAD,
        "the workspace already has {MAX_UNREAD} unread agent messages"
    );
    ensure!(
        usize::try_from(bytes)? + message.len() + attachments.len() <= MAX_UNREAD_BYTES,
        "the workspace's unread agent messages would exceed {MAX_UNREAD_BYTES} bytes"
    );
    tx.execute(
        "INSERT INTO agent_messages(workspace_id,created_at,message,attachments,wakes)
            VALUES (?1,?2,?3,?4,?5)",
        params![
            workspace_id,
            crate::time::UnixTime::now(),
            message,
            attachments,
            wakes
        ],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

/// Point the attached paths of messages in the workspaces of repository
/// `repository_id` from `source` to `destination`, in the transaction that
/// moves the repository's directory.
pub(crate) fn rebase_attachments(
    tx: &Transaction<'_>,
    repository_id: &RepositoryId,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let attached: Vec<(i64, String)> = tx
        .prepare(
            "SELECT m.id,m.attachments FROM agent_messages m
                JOIN workspaces w ON w.id=m.workspace_id
                WHERE w.repository_id=?1 AND m.attachments!='[]'",
        )?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rewrite_attachments(tx, "agent_messages", attached, source, destination)
}

/// Point the attached paths of the messages `attached` of `table`, by ID
/// with their JSON array of paths, from `source` to `destination`.
pub(crate) fn rewrite_attachments(
    tx: &Transaction<'_>,
    table: &str,
    attached: Vec<(i64, String)>,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    for (id, attachments) in attached {
        let paths: Vec<PathBuf> = serde_json::from_str::<Vec<PathBuf>>(&attachments)?
            .into_iter()
            .map(|path| match path.strip_prefix(source) {
                Ok(inside) => destination.join(inside),
                Err(_) => path,
            })
            .collect();
        tx.execute(
            &format!("UPDATE {table} SET attachments=?2 WHERE id=?1"),
            params![id, serde_json::to_string(&paths)?],
        )?;
    }
    Ok(())
}

/// A workspace's unread messages.
#[derive(Debug, Default)]
pub(crate) struct Unread {
    pub messages: Vec<AgentMessage>,
    /// Whether one of them starts an idle agent's next turn.
    pub wakes: bool,
}

impl Manager {
    /// Queue a message for the workspace's agents with copies of the files
    /// `attachments`, attached by `attacher`; without text, the files are the
    /// message. An identical unread message is not repeated.
    pub async fn send_agent_message(
        &self,
        selector: &SpaceSelector,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::agent_message(&message)?;
        }
        let workspace = self.workspace(selector).await?;
        if attachments.is_empty() {
            return self.queue_agent_message(&workspace.id, message).await;
        }
        let id = workspace.id.clone();
        let text = message.clone();
        self.queue_attached(workspace.into(), &text, attachments, |copies| {
            let attacher = Some(attacher.clone());
            self.queue_attached_message(&id, message, copies, attacher, Delivery::Wake)
        })
        .await
    }

    /// [`Self::send_to_space`] to the space `selector` names; the
    /// space it reached, by whether its agent runs, receives it once it next
    /// runs, or has no agent to read it until one starts there.
    pub async fn send_space_message(
        &self,
        selector: &SpaceSelector,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Reached> {
        let space = self.space(selector).await?;
        let reader = self.reader(&self.agents().await?, &space);
        self.send_to_space(&space, message, attachments, attacher)
            .await?;
        let mut reached = Reached::default();
        outcome(&mut reached, reader, Ok(())).space(&space);
        Ok(reached)
    }

    /// [`Self::send_agent_message`] to the workspace, or
    /// [`Self::send_chat_message`] to the chat, `space`.
    async fn send_to_space(
        &self,
        space: &PresentSpace,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<()> {
        let selector = SpaceSelector::id(space.id());
        match space {
            PresentSpace::Workspace(_) => {
                self.send_agent_message(&selector, message, attachments, attacher)
                    .await
            }
            PresentSpace::Chat(_) => {
                self.send_chat_message(&selector, message, attachments, attacher)
                    .await
            }
        }
    }

    /// [`Self::send_to_space`] to every workspace and chat with an
    /// agent, running, reattaching, stopped, queued or waiting for its
    /// restore, but the sending chat `sender_chat`, of `repository` when it
    /// names one, or only to the workspaces below `below`, the sender's own
    /// workspace; the spaces it reached, by whether their agent runs. An
    /// agent that does not run reads the message once it next runs, which
    /// the message does not cause, except for a restore waiting for an
    /// update. A space that refuses the message, such as one whose unread
    /// messages are full or that removal took meanwhile, is listed with why,
    /// and the others still receive it. Identical unread messages collapse,
    /// so sending again does not repeat it to recipients that have not read
    /// it yet.
    pub async fn send_all_message(
        &self,
        repository: Option<&Selector>,
        message: String,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
        below: Option<&SpaceId>,
        sender_chat: Option<&SpaceId>,
    ) -> Result<Reached> {
        let mut reached = Reached::default();
        for (space, reader) in self.recipients(repository, below, sender_chat).await? {
            let sent = self
                .send_to_space(&space, message.clone(), attachments.clone(), attacher)
                .await;
            outcome(&mut reached, reader, sent).space(&space);
        }
        Ok(reached)
    }

    /// The spaces with an agent, each with its [`Reader`]: the
    /// [`Self::workspace_recipients`] and, unless `below` names the sender's
    /// workspace, the [`Self::chat_recipients`] of `repository` when it
    /// names one.
    async fn recipients(
        &self,
        repository: Option<&Selector>,
        below: Option<&SpaceId>,
        sender_chat: Option<&SpaceId>,
    ) -> Result<Vec<(PresentSpace, Reader)>> {
        let repository = match repository {
            Some(repository) => Some(self.repository(repository.clone()).await?.id),
            None => None,
        };
        let workspaces = self
            .workspace_recipients(repository.as_ref(), below)
            .await?;
        let chats = match below {
            Some(_) => Vec::new(),
            None => {
                self.chat_recipients(repository.as_ref(), sender_chat)
                    .await?
            }
        };
        let agents = self.agents().await?;
        Ok((workspaces.into_iter().map(PresentSpace::Workspace))
            .chain(chats.into_iter().map(PresentSpace::Chat))
            .map(|space| {
                let reader = self.reader(&agents, &space);
                (space, reader)
            })
            .filter(|(_, reader)| *reader != Reader::Nobody)
            .collect())
    }

    /// The workspaces, except those being removed, of `repository` when it
    /// names one; only the workspaces below `below` when it names one.
    async fn workspace_recipients(
        &self,
        repository: Option<&RepositoryId>,
        below: Option<&SpaceId>,
    ) -> Result<Vec<Workspace>> {
        let mut workspaces = self.list_workspaces_within(below).await?;
        workspaces.retain(|workspace| {
            Some(&workspace.id) != below
                && repository.is_none_or(|repository| *repository == workspace.repository_id)
                && workspace.state != WorkspaceState::Removing
        });
        Ok(workspaces)
    }

    /// The chats, running or stopped, but `sender_chat`, by name, of
    /// `repository` when it names one.
    async fn chat_recipients(
        &self,
        repository: Option<&RepositoryId>,
        sender_chat: Option<&SpaceId>,
    ) -> Result<Vec<Chat>> {
        let mut chats = self.chats().await?;
        chats.retain(|chat| {
            repository.is_none_or(|repository| Some(repository) == chat.repository_id.as_ref())
                && sender_chat.is_none_or(|sender| *sender != chat.id)
        });
        chats.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(chats)
    }

    /// The workspaces whose agents run, and those with an agent's execution
    /// recorded or queued.
    async fn agents(&self) -> Result<WorkspaceAgents> {
        Ok(WorkspaceAgents {
            running: self.running_agent_workspaces().await,
            recorded: self.recorded_agent_spaces().await?,
            queued: self.queued_agent_workspaces().await?,
        })
    }

    /// Who reads a message queued for `space`, given the workspaces'
    /// `agents`: a chat always has an agent, which runs until it stops; a
    /// workspace's agent runs connected, or is reattaching, stopped, queued
    /// or waiting for its restore, or the workspace has none.
    fn reader(&self, agents: &WorkspaceAgents, space: &PresentSpace) -> Reader {
        match space {
            PresentSpace::Workspace(workspace) if agents.running.contains(&workspace.id) => {
                Reader::Now
            }
            PresentSpace::Workspace(workspace)
                if agents.recorded.contains(&workspace.id)
                    || agents.queued.contains(&workspace.id)
                    || self.has_saved_agent(&workspace.id) =>
            {
                Reader::OnStart
            }
            PresentSpace::Workspace(_) => Reader::Nobody,
            PresentSpace::Chat(chat) if chat.running => Reader::Now,
            PresentSpace::Chat(_) => Reader::OnStart,
        }
    }

    pub(crate) async fn queue_agent_message(
        &self,
        workspace_id: &SpaceId,
        message: String,
    ) -> Result<()> {
        self.queue_agent_message_delivered(workspace_id, message, Delivery::Wake)
            .await
    }

    /// Queue `message` with Shoal's copies `attachments` of files another
    /// space's files already keep, such as those a workspace sent its
    /// coordinator.
    pub(crate) async fn queue_agent_message_attached(
        &self,
        workspace_id: &SpaceId,
        message: String,
        attachments: Vec<PathBuf>,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .write(move |tx| {
                let queued = insert(tx, &workspace_id, &message, &attachments)?;
                Ok(queued.is_some())
            })
            .await?;
        if queued {
            self.signals.agent_message_queued();
        }
        Ok(())
    }

    pub(crate) async fn queue_agent_message_delivered(
        &self,
        workspace_id: &SpaceId,
        message: String,
        delivery: Delivery,
    ) -> Result<()> {
        self.queue_attached_message(workspace_id, message, Vec::new(), None, delivery)
            .await
    }

    /// Queue `message` with Shoal's copies `attachments`, which the
    /// workspace's attached files list from then on as attached by
    /// `attacher`.
    async fn queue_attached_message(
        &self,
        workspace_id: &SpaceId,
        message: String,
        attachments: Vec<Copy>,
        attacher: Option<Attacher>,
        delivery: Delivery,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .write(move |tx| {
                let paths: Vec<_> = attachments.iter().map(|copy| copy.path.clone()).collect();
                let queued = insert_delivered(tx, &workspace_id, &message, &paths, delivery)?;
                workspace_files::record(
                    tx,
                    &workspace_id,
                    workspace_files::FileSource::Message,
                    attacher.as_ref(),
                    &attachments,
                )?;
                Ok(queued.is_some())
            })
            .await?;
        if queued {
            self.signals.agent_message_queued();
        }
        Ok(())
    }

    /// [`Self::agent_messages`] of the workspace, or [`Self::chat_messages`]
    /// of the running chat, `selector` names.
    pub async fn space_messages(&self, selector: &SpaceSelector) -> Result<Vec<AgentMessage>> {
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.agent_messages(&SpaceSelector::id(&workspace.id)).await
            }
            PresentSpace::Chat(chat) => self.chat_messages(&chat.id).await,
        }
    }

    /// Unread messages, oldest first. They stay unread until delivered.
    pub async fn agent_messages(&self, selector: &SpaceSelector) -> Result<Vec<AgentMessage>> {
        let workspace = self.workspace(selector).await?;
        Ok(self.read_unread(workspace.id).await?.messages)
    }

    /// Unread messages, oldest first, and whether one of them wakes an idle
    /// agent.
    pub(crate) async fn unread_agent_messages(&self, selector: &SpaceSelector) -> Result<Unread> {
        let workspace = self.workspace(selector).await?;
        self.read_unread(workspace.id).await
    }

    /// The workspace's unread messages.
    async fn read_unread(&self, workspace_id: SpaceId) -> Result<Unread> {
        self.store
            .run(move |db| {
                let mut unread = Unread::default();
                let mut rows = db.prepare(
                    "SELECT id,created_at,message,attachments,wakes FROM agent_messages
                        WHERE workspace_id=?1 ORDER BY id",
                )?;
                let mut rows = rows.query([workspace_id])?;
                while let Some(row) = rows.next()? {
                    unread.messages.push(AgentMessage {
                        id: row.get("id")?,
                        created_at: row.get("created_at")?,
                        message: row.get("message")?,
                        attachments: crate::daemon::store::json_column(row, "attachments")?
                            .unwrap_or_default(),
                    });
                    unread.wakes |= row.get::<_, bool>("wakes")?;
                }
                Ok(unread)
            })
            .await
    }

    /// Acknowledge the item updates, messages and leftovers the agent of the
    /// workspace or running chat `selector` names received.
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
        self.mark_space_messages_delivered(selector, messages)
            .await?;
        self.acknowledge_leftovers(selector, leftovers).await
    }

    /// [`Self::mark_agent_messages_delivered`] for the workspace, or
    /// [`Self::mark_chat_messages_delivered`] for the running chat,
    /// `selector` names.
    pub async fn mark_space_messages_delivered(
        &self,
        selector: &SpaceSelector,
        ids: Vec<i64>,
    ) -> Result<()> {
        match self.space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.mark_agent_messages_delivered(&SpaceSelector::id(&workspace.id), ids)
                    .await
            }
            PresentSpace::Chat(chat) => self.mark_chat_messages_delivered(&chat.id, ids).await,
        }
    }

    /// Delete exactly these messages once an agent has received them, with
    /// the questions whose answers they carried.
    pub async fn mark_agent_messages_delivered(
        &self,
        selector: &SpaceSelector,
        ids: Vec<i64>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.store
            .write(move |tx| {
                for id in ids {
                    tx.execute(
                        "DELETE FROM agent_messages WHERE id=?1 AND workspace_id=?2",
                        params![id, workspace.id],
                    )?;
                    crate::daemon::questions::delete_delivered(tx, &workspace.id, id)?;
                }
                Ok(())
            })
            .await?;
        self.signals.agent_changed();
        Ok(())
    }
}

/// The workspaces whose agents run, and those with an agent's execution
/// recorded, such as one whose wrapper waits to reattach, or queued to start
/// or restore.
struct WorkspaceAgents {
    running: Vec<SpaceId>,
    recorded: HashSet<SpaceId>,
    queued: HashSet<SpaceId>,
}

/// Who reads a message queued for a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    /// Its running agent, now.
    Now,
    /// Its agent, once it next runs.
    OnStart,
    /// No agent, until one starts there.
    Nobody,
}

/// Where a space goes in `reached` once a message to it was `sent`: among
/// those that refused it, or else by its `reader`.
fn outcome(reached: &mut Reached, reader: Reader, sent: Result<()>) -> Outcome<'_> {
    match (sent, reader) {
        (Err(error), _) => Outcome::Refused(&mut reached.refused, format!("{error:#}")),
        (Ok(()), Reader::Now) => Outcome::Reached(&mut reached.now),
        (Ok(()), Reader::OnStart) => Outcome::Reached(&mut reached.on_start),
        (Ok(()), Reader::Nobody) => Outcome::Reached(&mut reached.without_agent),
    }
}

/// The list of a [`Reached`] that a space joins.
enum Outcome<'a> {
    Reached(&'a mut ReachedSpaces),
    Refused(&'a mut Refused, String),
}

impl Outcome<'_> {
    /// Add `space` to the list among its kind's.
    fn space(self, space: &PresentSpace) {
        let recipient = recipient(space);
        match (self, space) {
            (Self::Reached(spaces), PresentSpace::Workspace(_)) => {
                spaces.workspaces.push(recipient)
            }
            (Self::Reached(spaces), PresentSpace::Chat(_)) => spaces.chats.push(recipient),
            (Self::Refused(refused, error), PresentSpace::Workspace(_)) => {
                refused.workspaces.push(refusal(recipient, error))
            }
            (Self::Refused(refused, error), PresentSpace::Chat(_)) => {
                refused.chats.push(refusal(recipient, error))
            }
        }
    }
}

/// `space` as a message's result names it.
fn recipient(space: &PresentSpace) -> Recipient {
    Recipient {
        number: space.number(),
        id: space.id().clone(),
        name: space.name().clone(),
    }
}

/// `space` refusing a message for `error`, which the daemon logs.
fn refusal(space: Recipient, error: String) -> RefusedSpace {
    crate::daemon::log!("message to every agent refused by {}: {error}", space.id);
    RefusedSpace { space, error }
}

#[cfg(test)]
mod tests;
