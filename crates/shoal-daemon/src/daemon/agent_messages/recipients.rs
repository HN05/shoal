//! Sending a message to a space by number or ID, or to every space with an
//! agent, and reporting whom it reached.
use std::collections::HashSet;

use anyhow::Result;

use crate::{
    daemon::{
        workspace::Manager,
        workspace_files::{Attacher, NewFile},
    },
    model::{Space, SpaceKind},
    repository::Selector,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent_messages::{Reached, ReachedSpaces, Recipient, Refused, RefusedSpace},
    space_names::SpaceSelector,
};

impl Manager {
    /// [`Self::send_to`] to the space `selector` names; the
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
        self.send_to(space.clone(), message, attachments, attacher)
            .await?;
        let mut reached = Reached::default();
        outcome(&mut reached, reader, Ok(())).space(&space);
        Ok(reached)
    }

    /// [`Self::send_to`] to every workspace and chat with an
    /// agent, running, reattaching, stopped, queued or waiting for its
    /// restore, but the sender's own space `sender`, of `repository` when
    /// it names one, or only to the spaces below `below`, the sender's own
    /// space; the spaces it reached, by whether their agent runs. An
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
        sender: Option<&SpaceId>,
    ) -> Result<Reached> {
        let mut reached = Reached::default();
        for (space, reader) in self.recipients(repository, below, sender).await? {
            let sent = self
                .send_to(
                    space.clone(),
                    message.clone(),
                    attachments.clone(),
                    attacher,
                )
                .await;
            outcome(&mut reached, reader, sent).space(&space);
        }
        Ok(reached)
    }

    /// The spaces with an agent, each with its [`Reader`], by name: those
    /// [`Self::spaces_within`] `below` lists, but `sender` and the spaces
    /// being removed, of `repository` when it names one.
    async fn recipients(
        &self,
        repository: Option<&Selector>,
        below: Option<&SpaceId>,
        sender: Option<&SpaceId>,
    ) -> Result<Vec<(Space, Reader)>> {
        let repository = match repository {
            Some(repository) => Some(self.repository(repository.clone()).await?.id),
            None => None,
        };
        let mut spaces = self.spaces_within(below).await?;
        spaces.retain(|space| self.addressed(space, repository.as_ref(), sender));
        spaces.sort_by(|a, b| a.name.cmp(&b.name));
        let agents = self.agents().await?;
        Ok((spaces.into_iter())
            .map(|space| {
                let reader = self.reader(&agents, &space);
                (space, reader)
            })
            .filter(|(_, reader)| *reader != Reader::Nobody)
            .collect())
    }

    /// Whether a message to every agent from `sender`, of `repository` when
    /// it names one, goes to `space`: one of that repository, but the
    /// sender's own, that no removal is taking.
    fn addressed(
        &self,
        space: &Space,
        repository: Option<&RepositoryId>,
        sender: Option<&SpaceId>,
    ) -> bool {
        Some(&space.id) != sender
            && repository.is_none_or(|repository| Some(repository) == space.repository_id.as_ref())
            && !self.being_removed(space)
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
    fn reader(&self, agents: &WorkspaceAgents, space: &Space) -> Reader {
        let id = &space.id;
        match space.kind() {
            SpaceKind::Workspace if agents.running.contains(id) => Reader::Now,
            SpaceKind::Workspace
                if agents.recorded.contains(id)
                    || agents.queued.contains(id)
                    || self.has_saved_agent(id) =>
            {
                Reader::OnStart
            }
            SpaceKind::Workspace => Reader::Nobody,
            SpaceKind::Chat if space.running => Reader::Now,
            SpaceKind::Chat => Reader::OnStart,
        }
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
    fn space(self, space: &Space) {
        let recipient = recipient(space);
        match (self, space.kind()) {
            (Self::Reached(spaces), SpaceKind::Workspace) => spaces.workspaces.push(recipient),
            (Self::Reached(spaces), SpaceKind::Chat) => spaces.chats.push(recipient),
            (Self::Refused(refused, error), SpaceKind::Workspace) => {
                refused.workspaces.push(refusal(recipient, error))
            }
            (Self::Refused(refused, error), SpaceKind::Chat) => {
                refused.chats.push(refusal(recipient, error))
            }
        }
    }
}

/// `space` as a message's result names it.
fn recipient(space: &Space) -> Recipient {
    Recipient {
        number: space.number,
        id: space.id.clone(),
        name: space.name.clone(),
    }
}

/// `space` refusing a message for `error`, which the daemon logs.
fn refusal(space: Recipient, error: String) -> RefusedSpace {
    crate::daemon::log!("message to every agent refused by {}: {error}", space.id);
    RefusedSpace { space, error }
}
