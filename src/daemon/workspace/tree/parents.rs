//! Giving a workspace the space it is opened below: the chat or workspace
//! whose agent opened it, or the user when one opened it outside any.
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};

use super::super::{Manager, NewWorkspace};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{chats::Removing, spaces, store},
    model::{OpenedWorkspace, SpaceKind, Workspace},
};

impl Manager {
    /// Give `workspace` to the space `parent` that opened it, or to the user
    /// when it was opened outside one or the parent is gone. A workspace
    /// split from another stays below it.
    pub async fn assign_parent(&self, workspace: &str, parent: Option<String>) -> Result<()> {
        let workspace = workspace.to_owned();
        let held = [self.chat_removals.clone(), self.chat_owners.clone()];
        self.store
            .run(move |db| assign(db, &workspace, parent.as_deref(), &held))
            .await
    }

    /// [`Self::open_branch`] below `parent`, where and as `new` says,
    /// returning the workspace with its parent. A `confined` parent, whose
    /// own agent asks, takes over no existing workspace outside its subtree.
    pub async fn open_branch_below(
        &self,
        repository: crate::repository::Selector,
        branch: &str,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<String>,
        confined: bool,
    ) -> Result<OpenedWorkspace> {
        self.ensure_parent_repository(&repository, parent.as_deref())
            .await?;
        let new = NewWorkspace {
            parent: parent.clone(),
            ..new
        };
        let mut opened = self.open_branch_with(repository, branch, base, new).await?;
        if let Some(parent) = parent.as_deref().filter(|_| confined && opened.reused) {
            ensure!(
                self.within_subtree(parent, &opened.workspace.id).await?,
                "{} is outside the space opening it; ask the user to open it",
                opened.workspace.name
            );
        }
        opened.workspace = self
            .give_to_parent(&SpaceSelector::id(&opened.workspace.id), parent)
            .await?;
        Ok(opened)
    }

    /// [`Self::create_workspace`] below `parent`, returning the workspace with
    /// its parent.
    pub async fn create_workspace_below(
        &self,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<String>,
    ) -> Result<Workspace> {
        self.ensure_parent_repository(&repository, parent.as_deref())
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
        parent: Option<&str>,
    ) -> Result<()> {
        self.ensure_parent_takes(Some(repository), parent).await
    }

    /// Refuse a space of the repository `repository` selects, or of none for
    /// a chat without one, below `parent` when the parent cannot take it now.
    pub(crate) async fn ensure_parent_takes(
        &self,
        repository: Option<&crate::repository::Selector>,
        parent: Option<&str>,
    ) -> Result<()> {
        let Some(parent) = parent.map(str::to_owned) else {
            return Ok(());
        };
        let repository = match repository {
            Some(repository) => Some(self.repository(repository.clone()).await?.id),
            None => None,
        };
        self.store
            .run(move |db| {
                super::ready_parent(db, repository.as_deref(), &parent)?;
                Ok(())
            })
            .await
    }

    /// Assign `workspace` to `parent` and read it again.
    pub(crate) async fn give_to_parent(
        &self,
        workspace: &SpaceSelector,
        parent: Option<String>,
    ) -> Result<Workspace> {
        let id = self.workspace(workspace).await?.id;
        self.assign_parent(&id, parent).await?;
        self.workspace_by_id(&id).await
    }
}

/// An unknown parent, or a chat being removed, leaves the workspace to the
/// user. A workspace below a chat being removed stays there: the removal
/// listed it after reserving the chat, on this one thread, and takes it.
fn assign(
    db: &mut Connection,
    workspace: &str,
    parent: Option<&str>,
    [removing, owners]: &[Removing; 2],
) -> Result<()> {
    let tx = db.transaction()?;
    // A removal decided by the space the workspace is below keeps it there.
    ensure!(
        !store::exists(
            &tx,
            "SELECT 1 FROM workspaces WHERE id=?1 AND state=?2",
            params![workspace, crate::state::WorkspaceState::Removing]
        )?,
        "the workspace is being removed"
    );
    let current = super::parent_id(&tx, workspace)?;
    if let Some(current) = &current
        && spaces::space_kind(&tx, current)? == Some(SpaceKind::Workspace)
    {
        return Ok(());
    }
    if let Some(current) = current
        .as_deref()
        .filter(|current| Some(*current) != parent)
    {
        ensure!(
            !removing.reserved(current) && !owners.reserved(current),
            "a removal of the chat that opened the workspace, or of its workspaces, holds it"
        );
    }
    // A chat being removed takes no new workspace, nor a space gone; one
    // already below it stays there.
    let parent = match parent
        .filter(|parent| !removing.reserved(parent) || current.as_deref() == Some(*parent))
    {
        Some(parent) if spaces::space_kind(&tx, parent)?.is_some() => Some(parent),
        _ => None,
    };
    if parent != current.as_deref() {
        if let Some(parent) = parent {
            let repository: String = tx.query_row(
                "SELECT repository_id FROM workspaces WHERE id=?1",
                [workspace],
                |row| row.get(0),
            )?;
            super::ensure_parent(&tx, Some(&repository), workspace, parent)?;
        }
        super::set_parent(&tx, workspace, parent)?;
    }
    tx.commit()?;
    Ok(())
}
