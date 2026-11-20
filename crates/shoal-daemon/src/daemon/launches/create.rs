//! A workspace a start creates without starting an agent there, as
//! `shoal create` asks: placed as a launch's new workspace is, from what
//! its launch names, or the existing workspace that has the branch it
//! names, reopened.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Result, bail};
use shoal_protocol::launches::{Launch, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use super::{
    shape::Shape,
    space::{Follows, Placed, PlacedHome, Placement},
};
use crate::{
    config::modes::ModeName,
    daemon::{manager::Manager, scope::Caller},
    launches,
    model::SpaceKind,
    protocol::operations::{StartedWorkspace, WorkspaceStart},
};

impl Manager {
    /// Create the workspace `launch` describes for `caller`, starting no
    /// agent there, a new one below the space the launch names, else
    /// below `below`, and start preparing it with `env`. Only a parent the
    /// launch names refuses the existing workspace of the branch it names.
    pub(crate) async fn create_placed_workspace(
        self: &Arc<Self>,
        mut launch: Launch,
        (below, env): (Option<SpaceSelector>, Option<BTreeMap<String, String>>),
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        ensure_creates(&launch)?;
        launches::ensure_copy_fits(&launch)?;
        self.select_launch_spaces(&mut launch).await?;
        launch.setup_env = env;
        self.default_repository(&mut launch, below.as_ref()).await?;
        let shape = Shape {
            kind: SpaceKind::Workspace,
            mode: ModeName::DEFAULT,
            repository: true,
        };
        let placement = match self
            .workspace_placement(&launch, (&shape, true), caller)
            .await?
        {
            Placed::New(placement) => placement,
            Placed::Worker(worker) => self.reopened_placement(&worker).await?,
            Placed::Chat(..) => bail!(
                "a workspace starts on a branch; name it with --branch, an issue, a PR or branch link, or a copy"
            ),
        };
        if !reopens(&placement) {
            launch.parent = launch.parent.or(below);
        }
        let follows = Follows {
            agent: None,
            unfollowed: false,
        };
        let started = self.start_placed(launch, placement, follows, caller);
        started.await?.into_workspace()
    }

    /// Name the repository of the space `launch` goes below, else that of
    /// `below`, where a child goes, when the launch names none.
    async fn default_repository(
        &self,
        launch: &mut Launch,
        below: Option<&SpaceSelector>,
    ) -> Result<()> {
        let names = launches::names_repository(launch)?;
        let Some(parent) = launch.parent.as_ref().or(below).filter(|_| !names) else {
            return Ok(());
        };
        let repository = self.parent_repository(parent).await?.registered();
        launch.repository = repository.map(|id| id.to_string());
        Ok(())
    }

    /// The placement that reopens the existing workspace `worker` names as
    /// the one its launch starts in.
    async fn reopened_placement(&self, worker: &Launch) -> Result<Placement> {
        let Some(SpaceChoice::Space(selector)) = &worker.in_workspace else {
            bail!("the launch names no workspace");
        };
        let workspace = self.workspace(selector).await?;
        let opening = crate::start_plan::Opening {
            start: WorkspaceStart::Workspace(selector.clone()),
            base: None,
            path: None,
            git_profile: None,
            options: Default::default(),
            pr: None,
        };
        Ok(Placement {
            home: PlacedHome::Workspace(workspace.repository().clone(), opening),
            issue: None,
            blockers: Vec::new(),
        })
    }
}

/// Whether `placement` reopens a recorded workspace, which keeps its
/// place.
fn reopens(placement: &Placement) -> bool {
    matches!(
        &placement.home,
        PlacedHome::Workspace(_, opening) if matches!(opening.start, WorkspaceStart::Workspace(_))
    )
}

/// Refuse what `launch` names that only an agent's start takes: a
/// creation starts none.
fn ensure_creates(launch: &Launch) -> Result<()> {
    let agent_flags = [
        ("--agent", launch.agent.is_some()),
        ("--mode", launch.mode.is_some()),
        ("--model", !launch.model.is_empty()),
        ("--fork", launch.fork.is_some()),
        ("--permission", !launch.permissions.is_empty()),
        ("--add-blocker", !launch.blockers.is_empty()),
        ("--now", launch.now),
        ("--here", launch.here),
        ("--in", launch.in_workspace.is_some()),
        ("a task", !launch.task.is_empty() || launch.prompt.is_some()),
        ("a setup", launch.setup.is_some()),
    ];
    match agent_flags.iter().find(|(_, set)| *set) {
        Some((flag, _)) => {
            bail!(
                "{flag} applies to an agent, which a creation starts none of; `shoal start` starts one"
            )
        }
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
