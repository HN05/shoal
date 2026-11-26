//! Checking a launch before a start's pickers ask for the rest, as
//! [`Spaces::launch`] settles it, short of recording or fetching anything.
//! A launch whose kind is decided checks that kind's whole placement; one
//! whose mode a terminal has yet to pick checks what every kind shares.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, SpaceChoice, StartTarget};

use super::shape::Shape;
use crate::{
    model::{SpaceKind, Workspace},
    scope::Caller,
};
use shoal_core::launches;

impl Spaces {
    /// Refuses before anything is created. Returns the existing workspace the
    /// start goes into: the one `--in` names, or that has the branch it
    /// names. A launch that names another joined machine passes, since that
    /// machine checks it as it starts it.
    pub async fn check_launch(
        &self,
        daemon: &dyn Daemon,
        launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        self.check_starting(daemon, launch, Starts::Agent, caller)
            .await
    }

    /// As [`Self::check_launch`] checks a worker's start, except that a
    /// creation starts no agent, so the agent of an existing workspace it
    /// opens refuses nothing.
    pub async fn check_creation_launch(
        &self,
        daemon: &dyn Daemon,
        launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        self.check_starting(daemon, launch, Starts::Nothing, caller)
            .await
    }

    /// `starts` says what it starts in the workspace it goes into.
    async fn check_starting(
        &self,
        daemon: &dyn Daemon,
        mut launch: Launch,
        starts: Starts,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        if self.launches_elsewhere(daemon, &launch).await? {
            return Ok(None);
        }
        launches::ensure_copy_fits(&launch)?;
        launches::ensure_in_fits(&launch)?;
        self.select_launch_spaces(daemon, &mut launch).await?;
        self.settle_launch_run(daemon, &mut launch, caller).await?;
        let repository = self.checked_repository(daemon, &launch, caller).await?;
        (daemon.coordination().ensure_caller_holds(
            daemon,
            caller,
            &launch.permissions,
            repository.as_ref(),
        ))
        .await?;
        let shape = match launches::decides_kind(&launch) {
            true => self.checked_shape(daemon, &launch, caller).await?,
            false => None,
        };
        match shape {
            Some(shape) => {
                self.check_placement(daemon, &launch, (&shape, starts), caller)
                    .await
            }
            None => {
                self.check_shared(daemon, &launch, repository.as_ref(), caller)
                    .await
            }
        }
    }

    /// As [`Self::launch`] places it. `starts` says what it starts in the
    /// existing workspace it goes into.
    async fn check_placement(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        (shape, starts): (&Shape, Starts),
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        launches::ensure_fits(launch, &[], shape.kind, Some(&shape.mode))?;
        if shape.starts_worker(launch) {
            return self
                .check_worker(daemon, launch, shape, caller)
                .await
                .map(Some);
        }
        match (shape.kind, starts) {
            (SpaceKind::Chat, _) => {
                self.check_chat_placement(daemon, launch, shape, caller)
                    .await
            }
            (SpaceKind::Workspace, Starts::Agent) => {
                self.check_workspace_placement(daemon, launch, caller).await
            }
            (SpaceKind::Workspace, Starts::Nothing) => {
                self.check_workspace_creation(daemon, launch, caller).await
            }
        }
    }

    /// The parent it goes below, the blockers it names and the issue it
    /// starts on, which every kind reads. Returns the workspace it names
    /// with `--in`.
    async fn check_shared(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        repository: Option<&RepositoryId>,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        (self.check_launch_parent(daemon, launch, repository, caller)).await?;
        if let Some(repository) = repository {
            (self.named_blockers(daemon, repository, &launch.blockers, caller)).await?;
            self.check_launch_issue(daemon, launch, repository, caller)
                .await?;
        }
        match named_workspace(launch) {
            Some(workspace) => self.workspace(workspace).await.map(Some),
            None => Ok(None),
        }
    }

    /// Refuses an issue neither kind of space could read, as a chat reads
    /// it by its URL.
    async fn check_launch_issue(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(issue) = launches::named_issue(launch)? else {
            return Ok(());
        };
        let by_url = launch.name.is_some();
        (self
            .launch_issue(daemon, &issue, (repository, by_url), caller)
            .await)
            .map(drop)
    }

    /// Refuses the workspace whose worktree the space runs in, one outside a
    /// scoped caller's tree, and one that cannot take it now. A scoped
    /// caller's space goes below its own space unless it names another.
    pub(super) async fn check_launch_parent(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        repository: Option<&RepositoryId>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let parent = launch.parent.as_ref();
        (self.ensure_parent_not_host(daemon, named_workspace(launch), parent)).await?;
        let mut parent = parent.cloned();
        if let Some(caller) = caller {
            daemon
                .coordination()
                .parent_below(daemon, caller, &mut parent)
                .await?;
        }
        let parent = self.parent_space_id(daemon, parent.as_ref()).await?;
        let repository = repository.map(Into::into);
        (self.ensure_parent_takes(daemon, repository.as_ref(), parent.as_ref())).await
    }

    /// As far as the launch names or implies one without a picker: the
    /// repository of the workspace `--in` names, the one it names, refused
    /// when none is registered, else as
    /// [`launches::shape::launch_repository`] finds it.
    pub(super) async fn checked_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<RepositoryId>> {
        if let Some(workspace) = named_workspace(launch) {
            return Ok(Some(self.workspace(workspace).await?.repository().clone()));
        }
        let positional = match launches::decided_target(launch)? {
            Some(StartTarget::Repository(named)) => Some(named),
            _ => None,
        };
        if let Some(named) = (launch.repository.clone()).or(positional) {
            let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
            let repos = daemon.repositories().await?;
            return Ok(Some(
                crate::repository::select(&repos, selector)
                    .await?
                    .id
                    .clone(),
            ));
        }
        let state = self.launch_state(daemon, launch, caller).await?;
        let found = launches::shape::launch_repository(&state, launch).await?;
        Ok(found.map(|repository| repository.id))
    }
}

/// Its own agent for a start, which an agent of the workspace's own may
/// refuse, and nothing for a creation.
#[derive(Clone, Copy)]
enum Starts {
    Agent,
    Nothing,
}

/// Once named by ID.
fn named_workspace(launch: &Launch) -> Option<&shoal_protocol::space_names::SpaceSelector> {
    launch.in_workspace.as_ref().and_then(SpaceChoice::named)
}

#[cfg(test)]
mod tests;
