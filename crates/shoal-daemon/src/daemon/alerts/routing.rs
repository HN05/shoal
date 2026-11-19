//! Whom an alert reaches, the decision every alert takes: up from a space
//! to its coordinator or the user ([`Manager::route_up`]), or down to the
//! agents of spaces ([`Manager::route`]).
use std::collections::HashSet;

use anyhow::Result;

use crate::{
    daemon::{manager::Manager, notifications::Source, scope::Caller, tree::Receiver},
    model::{Space, SpaceKind},
    repository::Selector,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

/// Where an alert sent down goes.
pub(crate) enum Route<'a> {
    /// To the space `selector` names.
    Space(&'a SpaceSelector),
    /// To every space with an agent that `caller`'s scope reaches but
    /// its own, of `repository` when it names one.
    All {
        caller: Option<&'a Caller>,
        repository: Option<&'a Selector>,
    },
}

/// Whom an alert reaches.
pub(crate) enum Recipients {
    /// The user.
    User,
    /// The coordinator's space on this machine.
    Coordinator(SpaceId),
    /// The first space above on another machine, which passes it on to the
    /// coordinator there.
    Above(SpaceId),
    /// The agents of these spaces, each with who reads it.
    Spaces(Vec<(Space, Reader)>),
}

/// Who reads an alert queued for a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reader {
    /// Its running agent, now.
    Now,
    /// Its agent, once it next runs.
    OnStart,
    /// No agent, until one starts there.
    Nobody,
}

impl Manager {
    /// Whom an alert sent down along `route` reaches.
    pub(crate) async fn route(&self, route: Route<'_>) -> Result<Recipients> {
        match route {
            Route::Space(selector) => {
                let space = self.space(selector).await?;
                let reader = self.reader(&self.agents().await?, &space);
                Ok(Recipients::Spaces(vec![(space, reader)]))
            }
            Route::All { caller, repository } => {
                let below = self.reach_root(caller).await?;
                let own = caller.map(|caller| &caller.space_id);
                let spaces = self.agents_within(repository, below.as_ref(), own).await?;
                Ok(Recipients::Spaces(spaces))
            }
        }
    }

    /// Whom an alert sent up from the space `from`, about an event from
    /// `source`, reaches: the agent of the nearest space above with an
    /// agent, here or on another machine, or else the user, as when the
    /// event comes from an agent and the space has none of its own, since
    /// the coordinator itself or the user acted there.
    pub(crate) async fn route_up(&self, from: &SpaceId, source: Source) -> Result<Recipients> {
        let (id, agents) = (from.clone(), self.tracked_agents().await);
        let receiver = (self.store)
            .run(move |db| crate::daemon::tree::coordinator(db, Some(id), source, &agents))
            .await?;
        Ok(match receiver {
            Some(Receiver::Here(coordinator)) => Recipients::Coordinator(coordinator),
            Some(Receiver::Elsewhere(above)) => Recipients::Above(above),
            None => Recipients::User,
        })
    }

    /// The spaces with an agent, each with its [`Reader`], by name: those
    /// [`Self::spaces_within`] `below` lists, but `sender` and the spaces
    /// being removed, of `repository` when it names one.
    async fn agents_within(
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

    /// Whether an alert to every agent from `sender`, of `repository` when
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

    /// Who reads an alert queued for `space`, given the workspaces'
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
