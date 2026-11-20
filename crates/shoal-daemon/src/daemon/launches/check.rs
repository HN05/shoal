//! Checking a launch before a start's pickers ask for the rest: what the
//! daemon would refuse of it, settled as [`Manager::launch`] settles it
//! short of recording or fetching anything, whatever kind of space it
//! starts in. A launch whose kind is decided checks that kind's whole
//! placement; one whose mode a terminal has yet to pick checks what every
//! kind shares.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, SpaceChoice, StartTarget};

use super::shape::Shape;
use crate::{
    daemon::{manager::Manager, scope::Caller},
    launches,
    model::{SpaceKind, Workspace},
};

impl Manager {
    /// Refuse what starting `launch` for `caller` would refuse, before
    /// anything is created, returning instead the existing workspace the
    /// start goes into: the one it names with `--in`, or that has the
    /// branch it names. A launch that names another joined machine passes,
    /// since that machine checks it as it starts it.
    pub(crate) async fn check_launch(
        &self,
        mut launch: Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        if self.launches_elsewhere(&launch).await? {
            return Ok(None);
        }
        launches::ensure_copy_fits(&launch)?;
        launches::ensure_in_fits(&launch)?;
        self.select_launch_spaces(&mut launch).await?;
        let repository = self.checked_repository(&launch, caller).await?;
        (self.ensure_caller_holds(caller, &launch.permissions, repository.as_ref())).await?;
        match launches::decides_kind(&launch) {
            true => {
                let shape = self.launch_shape(&launch, caller).await?;
                self.check_placement(&launch, &shape, caller).await
            }
            false => {
                self.check_shared(&launch, repository.as_ref(), caller)
                    .await
            }
        }
    }

    /// Refuse what placing `launch` in `shape` for `caller` would refuse,
    /// as [`Self::launch`] places it.
    async fn check_placement(
        &self,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        launches::ensure_fits(launch, &[], shape.kind, Some(&shape.mode))?;
        if shape.starts_worker(launch) {
            return self.check_worker(launch, shape, caller).await.map(Some);
        }
        match shape.kind {
            SpaceKind::Chat => self.check_chat_placement(launch, shape, caller).await,
            SpaceKind::Workspace => self.check_workspace_placement(launch, caller).await,
        }
    }

    /// Refuse what every kind of space `launch` could start in for
    /// `caller` refuses, in `repository`, if any: the parent it goes below,
    /// the blockers it names and the issue it starts on, which every kind
    /// reads; returns the workspace it names with `--in`.
    async fn check_shared(
        &self,
        launch: &Launch,
        repository: Option<&RepositoryId>,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        (self.check_launch_parent(launch, repository, caller)).await?;
        if let Some(repository) = repository {
            (self.named_blockers(repository, &launch.blockers, caller)).await?;
            self.check_launch_issue(launch, repository, caller).await?;
        }
        match named_workspace(launch) {
            Some(workspace) => self.workspace(workspace).await.map(Some),
            None => Ok(None),
        }
    }

    /// Refuse the issue `launch` starts on in `repository` where neither
    /// kind of space could read it, as a chat it names reads it by its URL.
    async fn check_launch_issue(
        &self,
        launch: &Launch,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(issue) = launches::named_issue(launch)? else {
            return Ok(());
        };
        let by_url = launch.name.is_some();
        (self
            .launch_issue(&issue, (repository, by_url), caller)
            .await)
            .map(drop)
    }

    /// Refuse the parent `launch` names where its new space, in
    /// `repository` or without one, could not go below it for `caller`: the
    /// workspace whose worktree it runs in, one outside a scoped caller's
    /// tree, or one that cannot take it now. A scoped caller's space goes
    /// below its own space unless it names another.
    pub(super) async fn check_launch_parent(
        &self,
        launch: &Launch,
        repository: Option<&RepositoryId>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let parent = launch.parent.as_ref();
        (self.ensure_parent_not_host(named_workspace(launch), parent)).await?;
        let mut parent = parent.cloned();
        if let Some(caller) = caller {
            self.parent_below(caller, &mut parent).await?;
        }
        let parent = self.parent_space_id(parent.as_ref()).await?;
        let repository = repository.map(Into::into);
        (self.ensure_parent_takes(repository.as_ref(), parent.as_ref())).await
    }

    /// The registered repository `launch` starts in, as far as it names or
    /// implies one without a picker: the repository of the workspace it
    /// names with `--in`, the one it names, refused when none is
    /// registered, else as [`launches::shape::launch_repository`] finds it.
    async fn checked_repository(
        &self,
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
            let repos = self.repositories().await?;
            return Ok(Some(
                crate::repository::select(&repos, selector)
                    .await?
                    .id
                    .clone(),
            ));
        }
        let state = self.launch_state(launch, caller).await?;
        let found = launches::shape::launch_repository(&state, launch).await?;
        Ok(found.map(|repository| repository.id))
    }
}

/// The existing workspace `launch` names with `--in`, once named by ID.
fn named_workspace(launch: &Launch) -> Option<&shoal_protocol::space_names::SpaceSelector> {
    launch.in_workspace.as_ref().and_then(SpaceChoice::named)
}

#[cfg(test)]
mod tests;
