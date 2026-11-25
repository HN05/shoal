//! Whom an alert reaches, the decision every alert takes: up from a space
//! to its coordinator or the user ([`Coordination::route_up`]), or down to the
//! agents of spaces ([`Coordination::route`]).
use crate::{Coordination, daemon::Daemon};
use std::collections::HashSet;

use anyhow::Result;

use anyhow::bail;
use rusqlite::Connection;
use shoal_store::tree::{self, Receiver};

use crate::{
    alerts::{NewAlert, Sent},
    daemon::PassedUp,
    model::{Space, SpaceKind},
    notifications::Source,
    repository::Selector,
    scope::Caller,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

/// Where an alert sent down goes.
pub enum Route<'a> {
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
pub enum Recipients {
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
pub enum Reader {
    /// Its running agent, now.
    Now,
    /// Its agent, once it next runs.
    OnStart,
    /// No agent, until one starts there.
    Nobody,
}

impl Coordination {
    /// Queue `alert` about the space it names for its coordinator: the
    /// nearest space above it with an agent, which receives what arrived
    /// once a queued restore starts it, or, once the tree goes on above
    /// this machine, the nearest on the machines above, which the event
    /// passes up to. `for_user` says the user would receive the event
    /// without a coordinator, which the user then does if the coordinator's
    /// space goes before its agent reads it. Returns where it went, or none
    /// when the alert goes to the user, as [`Self::route`] decides, or its
    /// coordinator's alerts are full; the user then receives the event
    /// instead.
    pub async fn tell_coordinator(
        &self,
        daemon: &dyn Daemon,
        alert: NewAlert,
        for_user: bool,
        source: Source,
    ) -> Option<Sent> {
        let (Some(name), Some(id)) = (alert.about.name.clone(), alert.about.id.clone()) else {
            return None;
        };
        let told = match self.route_up(daemon, &id, source).await {
            Ok(Recipients::Coordinator(coordinator)) => {
                let queued = daemon.queue_alert(&coordinator, alert, for_user).await;
                queued.map(|id| Sent {
                    alerts: vec![id],
                    ..Sent::default()
                })
            }
            Ok(Recipients::Above(above)) => {
                (self.pass_up_alert(daemon, above, alert, for_user).await).map(|()| Sent {
                    passed_up: true,
                    ..Sent::default()
                })
            }
            Ok(Recipients::User | Recipients::Spaces(_)) => return None,
            Err(error) => Err(error),
        };
        match told {
            Ok(sent) => Some(sent),
            Err(error) => {
                crate::log!("message for the coordinator of {name}: {error:#}");
                None
            }
        }
    }

    /// Pass `alert`, about the space it names, up to the space `above`,
    /// which another machine runs, with the records of its files.
    async fn pass_up_alert(
        &self,
        daemon: &dyn Daemon,
        above: SpaceId,
        alert: NewAlert,
        for_user: bool,
    ) -> Result<()> {
        let (Some(name), Some(id)) = (&alert.about.name, &alert.about.id) else {
            bail!("an alert passed up names its space");
        };
        let mut files = daemon.sent_files(alert.attachments.clone()).await?;
        files.extend(alert.relayed.iter().cloned());
        let event = PassedUp {
            name,
            space: id,
            kind: alert.kind,
            for_user,
            message: alert.message.clone(),
            agent: None,
            files: &files,
        };
        // Boxed: on the hub, an event for its own machine's space comes back
        // here when no space there takes it.
        Box::pin(daemon.pass_up(above, event)).await
    }

    /// Whom an alert sent down along `route` reaches.
    pub async fn route(&self, daemon: &dyn Daemon, route: Route<'_>) -> Result<Recipients> {
        match route {
            Route::Space(selector) => {
                let space = daemon.space(selector).await?;
                let reader = self.reader(daemon, &self.agents(daemon).await?, &space);
                Ok(Recipients::Spaces(vec![(space, reader)]))
            }
            Route::All { caller, repository } => {
                let below = self.reach_root(caller).await?;
                let own = caller.map(|caller| &caller.space_id);
                let spaces = self
                    .agents_within(daemon, repository, below.as_ref(), own)
                    .await?;
                Ok(Recipients::Spaces(spaces))
            }
        }
    }

    /// Whom an alert sent up from the space `from`, about an event from
    /// `source`, reaches: the agent of the nearest space above with an
    /// agent, here or on another machine, or else the user, as when the
    /// event comes from an agent and the space has none of its own, since
    /// the coordinator itself or the user acted there.
    pub async fn route_up(
        &self,
        daemon: &dyn Daemon,
        from: &SpaceId,
        source: Source,
    ) -> Result<Recipients> {
        let (id, agents) = (from.clone(), daemon.tracked_agents().await);
        let receiver = (self.store)
            .run(move |db| coordinator(db, &id, source, &agents))
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
        daemon: &dyn Daemon,
        repository: Option<&Selector>,
        below: Option<&SpaceId>,
        sender: Option<&SpaceId>,
    ) -> Result<Vec<(Space, Reader)>> {
        let repository = match repository {
            Some(repository) => Some(daemon.repository(repository.clone()).await?.id),
            None => None,
        };
        let mut spaces = daemon.spaces_within(below).await?;
        spaces.retain(|space| self.addressed(daemon, space, repository.as_ref(), sender));
        spaces.sort_by(|a, b| a.name.cmp(&b.name));
        let agents = self.agents(daemon).await?;
        Ok((spaces.into_iter())
            .map(|space| {
                let reader = self.reader(daemon, &agents, &space);
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
        daemon: &dyn Daemon,
        space: &Space,
        repository: Option<&RepositoryId>,
        sender: Option<&SpaceId>,
    ) -> bool {
        Some(&space.id) != sender
            && repository.is_none_or(|repository| Some(repository) == space.repository_id.as_ref())
            && !daemon.being_removed(space)
    }

    /// The workspaces whose agents run, and those with an agent's execution
    /// recorded or queued.
    async fn agents(&self, daemon: &dyn Daemon) -> Result<WorkspaceAgents> {
        Ok(WorkspaceAgents {
            running: daemon.running_agent_workspaces().await,
            recorded: daemon.recorded_agent_spaces().await?,
            queued: daemon.queued_agent_workspaces().await?,
        })
    }

    /// Who reads an alert queued for `space`, given the workspaces'
    /// `agents`: a chat always has an agent, which runs until it stops; a
    /// workspace's agent runs connected, or is reattaching, stopped, queued
    /// or waiting for its restore, or the workspace has none.
    fn reader(&self, daemon: &dyn Daemon, agents: &WorkspaceAgents, space: &Space) -> Reader {
        let id = &space.id;
        match space.kind() {
            SpaceKind::Workspace if agents.running.contains(id) => Reader::Now,
            SpaceKind::Workspace
                if agents.recorded.contains(id)
                    || agents.queued.contains(id)
                    || daemon.has_saved_agent(id) =>
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

/// The coordinator of the space `id` for an event from `source`: the
/// nearest space above it with an agent, or the first one above on
/// another machine.
fn coordinator(
    db: &Connection,
    id: &SpaceId,
    source: Source,
    agents: &tree::TrackedAgents,
) -> Result<Option<Receiver>> {
    if source == Source::Agent && !crate::coordinators::has_running_agent(db, id, agents)? {
        return Ok(None);
    }
    if let Some(coordinator) = tree::coordinator_of(db, id, agents)? {
        return Ok(Some(Receiver::Here(coordinator)));
    }
    Ok(tree::first_absent_above(db, id)?.map(Receiver::Elsewhere))
}
