//! Giving a workspace the space it is opened below: the chat or workspace
//! whose agent opened it, or the user when one opened it outside any.
use anyhow::{Context as _, Result, ensure};
use rusqlite::params;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use super::super::{manager::Manager, workspace::NewWorkspace};
use crate::{
    daemon::{
        scope::Caller,
        spaces::{self, Removing},
        store,
    },
    model::{OpenedWorkspace, SpaceKind, Workspace},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// Give `workspace` to the space `parent` that opened it, or to the user
    /// when it was opened outside one or the parent is gone. A workspace
    /// split from another stays below it.
    pub async fn assign_parent(&self, workspace: &SpaceId, parent: Option<SpaceId>) -> Result<()> {
        let workspace = workspace.to_owned();
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        self.store
            .write(move |tx| assign(tx, &workspace, parent.as_ref(), &held))
            .await
    }

    /// [`Self::open_branch`] below `parent`, where and as `new` says,
    /// returning the workspace with its parent. A scoped `caller` takes
    /// over no existing workspace outside its scope.
    pub(crate) async fn open_branch_below(
        &self,
        repository: crate::repository::Selector,
        branch: &str,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<SpaceId>,
        caller: Option<&Caller>,
    ) -> Result<OpenedWorkspace> {
        self.ensure_parent_repository(&repository, parent.as_ref())
            .await?;
        let new = NewWorkspace {
            parent: parent.clone(),
            ..new
        };
        let mut opened = self.open_branch_with(repository, branch, base, new).await?;
        if opened.reused {
            (self.ensure_takes_over(&opened.workspace, caller)).await?;
        }
        opened.workspace = self
            .give_to_parent(&SpaceSelector::id(&opened.workspace.id), parent)
            .await?;
        Ok(opened)
    }

    /// Refuse a scoped `caller` the existing `workspace` outside its scope.
    pub(in crate::daemon) async fn ensure_takes_over(
        &self,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let space = crate::model::Space::from(workspace.clone());
        self.ensure_reaches(caller, &space).await
    }

    /// [`Self::create_workspace`] below `parent`, returning the workspace with
    /// its parent.
    pub(crate) async fn create_workspace_below(
        &self,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<SpaceId>,
    ) -> Result<Workspace> {
        self.ensure_parent_repository(&repository, parent.as_ref())
            .await?;
        let new = NewWorkspace {
            parent: parent.clone(),
            ..new
        };
        let workspace = self
            .create_workspace_with(repository, name, base, new)
            .await?;
        self.give_to_parent(&SpaceSelector::id(&workspace.id), parent)
            .await
    }

    /// Refuse a space below `parent` in another repository than the
    /// parent's, or below a workspace that is not ready, before creating
    /// anything.
    pub(crate) async fn ensure_parent_repository(
        &self,
        repository: &crate::repository::Selector,
        parent: Option<&SpaceId>,
    ) -> Result<()> {
        self.ensure_parent_takes(Some(repository), parent).await
    }

    /// Refuse a space of the repository `repository` selects, or of none for
    /// a chat without one, below `parent` when the parent cannot take it now,
    /// as this machine has it or, for one another joined machine runs, as
    /// the hub lists it.
    pub(crate) async fn ensure_parent_takes(
        &self,
        repository: Option<&crate::repository::Selector>,
        parent: Option<&SpaceId>,
    ) -> Result<()> {
        let Some(parent) = parent.cloned() else {
            return Ok(());
        };
        let repository = match repository {
            Some(repository) => Some(self.repository(repository.clone()).await?.id),
            None => None,
        };
        if self.find_space(&parent).await?.is_none() && self.hub.runs_elsewhere(self, &parent).await
        {
            let repository = repository.as_ref();
            return self
                .hub
                .ensure_parent_elsewhere(self, None, repository, &parent)
                .await;
        }
        self.store
            .run(move |db| {
                super::ready_parent(db, repository.as_ref(), &parent)?;
                Ok(())
            })
            .await
    }

    /// The ID of the space `selector` names as a new space's parent: one
    /// here, else one another joined machine runs, as the hub lists it.
    pub(crate) async fn parent_space_id(
        &self,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        let Some(selector) = selector else {
            return Ok(None);
        };
        match self.space(selector).await {
            Ok(space) => Ok(Some(space.id)),
            Err(error) => match self.hub.machine_of(self, selector).await {
                Ok(Some((_, listed))) => Ok(Some(listed.space.id)),
                _ => Err(error),
            },
        }
    }

    /// The repository of the space `selector` names as a new space's
    /// parent, which a child shares: its own here, or for one another
    /// joined machine runs, this machine's registration of the repository
    /// the hub lists it in.
    pub(crate) async fn parent_repository(
        &self,
        selector: &SpaceSelector,
    ) -> Result<Option<RepositoryId>> {
        let id = (self.parent_space_id(Some(selector)).await?).context("a parent names a space")?;
        match self.find_space(&id).await? {
            Some(space) => Ok(space.repository_id),
            None => self.hub.repository_elsewhere(self, &id).await,
        }
    }

    /// Assign `workspace` to `parent` and read it again.
    pub(crate) async fn give_to_parent(
        &self,
        workspace: &SpaceSelector,
        parent: Option<SpaceId>,
    ) -> Result<Workspace> {
        let id = self.workspace(workspace).await?.id;
        self.assign_parent(&id, parent).await?;
        self.workspace_by_id(&id).await
    }
}

/// An unknown parent, or a chat being removed, leaves the workspace to the
/// user. A workspace below a chat being removed stays there: the removal
/// listed it after reserving the chat, on this one thread, and takes it. A
/// parent the workspace already has stays, as one another joined machine
/// runs, which its creation recorded once the hub listed it.
fn assign(
    tx: &rusqlite::Transaction<'_>,
    workspace: &SpaceId,
    parent: Option<&SpaceId>,
    [removing, owners]: &[Removing; 2],
) -> Result<()> {
    // A removal decided by the space the workspace is below keeps it there.
    ensure!(
        !store::exists(
            tx,
            "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
            params![workspace, crate::state::WorkspaceState::Removing]
        )?,
        "the workspace is being removed"
    );
    let current = super::parent_id(tx, workspace)?;
    if parent.is_some() && parent == current.as_ref() {
        return Ok(());
    }
    if let Some(current) = &current
        && spaces::space_kind(tx, current)? == Some(SpaceKind::Workspace)
    {
        return Ok(());
    }
    if let Some(current) = current.as_ref().filter(|current| Some(*current) != parent) {
        ensure!(
            !removing.reserved(current) && !owners.reserved(current),
            "a removal of the chat that opened the workspace, or of its workspaces, holds it"
        );
    }
    // A chat being removed takes no new workspace, nor a space gone; one
    // already below it stays there.
    let parent = match parent
        .filter(|parent| !removing.reserved(parent) || current.as_ref() == Some(*parent))
    {
        Some(parent) if spaces::space_kind(tx, parent)?.is_some() => Some(parent),
        _ => None,
    };
    if parent != current.as_ref() {
        if let Some(parent) = parent {
            let repository: RepositoryId = tx.query_row(
                "SELECT repository_id FROM spaces WHERE id=?1",
                [workspace],
                |row| row.get(0),
            )?;
            super::ensure_parent(tx, Some(&repository), workspace, parent)?;
        }
        super::set_parent(tx, workspace, parent)?;
    }
    Ok(())
}
