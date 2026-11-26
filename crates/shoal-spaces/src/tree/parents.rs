//! Giving a workspace the space it is opened below: the chat or workspace
//! whose agent opened it, or the user when it was opened outside any.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, ensure};
use rusqlite::params;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use super::super::workspace::NewWorkspace;
use crate::{
    model::{OpenedWorkspace, SpaceKind, Workspace},
    {
        scope::Caller,
        spaces::{self, Removing},
        store,
    },
};
use shoal_core::launches::SpaceRepository;
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Gives it to the user when it was opened outside a space or the parent
    /// is gone. A workspace split from another stays below it.
    pub async fn assign_parent(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceId,
        parent: Option<SpaceId>,
    ) -> Result<()> {
        let workspace = workspace.to_owned();
        let held = [self.space_removals.clone(), self.completed_owners.clone()];
        let elsewhere = match &parent {
            Some(parent) => {
                self.find_space(parent).await?.is_none() && daemon.runs_elsewhere(parent).await
            }
            None => false,
        };
        self.store
            .write(move |tx| assign(tx, &workspace, (parent.as_ref(), elsewhere), &held))
            .await
    }

    /// [`Self::open_branch_with`] below `parent`. A scoped `caller` takes over no
    /// existing workspace outside its scope.
    #[expect(
        clippy::too_many_arguments,
        reason = "what the branch opens with and below, beside the daemon"
    )]
    pub async fn open_branch_below(
        &self,
        daemon: &dyn Daemon,
        repository: crate::repository::Selector,
        branch: &str,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<SpaceId>,
        caller: Option<&Caller>,
    ) -> Result<OpenedWorkspace> {
        self.ensure_parent_repository(daemon, &repository, parent.as_ref())
            .await?;
        let new = NewWorkspace {
            parent: parent.clone(),
            ..new
        };
        let mut opened = self
            .open_branch_with(daemon, repository, branch, base, new)
            .await?;
        if opened.reused {
            (self.ensure_takes_over(daemon, &opened.workspace, caller)).await?;
        }
        opened.workspace = self
            .give_to_parent(daemon, &SpaceSelector::id(&opened.workspace.id), parent)
            .await?;
        Ok(opened)
    }

    pub async fn ensure_takes_over(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let space = crate::model::Space::from(workspace.clone());
        daemon
            .coordination()
            .ensure_reaches(daemon, caller, &space)
            .await
    }

    pub async fn create_workspace_below(
        &self,
        daemon: &dyn Daemon,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        new: NewWorkspace<'_>,
        parent: Option<SpaceId>,
    ) -> Result<Workspace> {
        self.ensure_parent_repository(daemon, &repository, parent.as_ref())
            .await?;
        let new = NewWorkspace {
            parent: parent.clone(),
            ..new
        };
        let workspace = self
            .create_workspace_with(daemon, repository, name, base, new)
            .await?;
        self.give_to_parent(daemon, &SpaceSelector::id(&workspace.id), parent)
            .await
    }

    /// Refuses before creating anything.
    pub async fn ensure_parent_repository(
        &self,
        daemon: &dyn Daemon,
        repository: &crate::repository::Selector,
        parent: Option<&SpaceId>,
    ) -> Result<()> {
        self.ensure_parent_takes(daemon, Some(repository), parent)
            .await
    }

    /// Judges a parent another joined machine runs as the hub lists it. A
    /// chat without a repository has none.
    pub async fn ensure_parent_takes(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&crate::repository::Selector>,
        parent: Option<&SpaceId>,
    ) -> Result<()> {
        let Some(parent) = parent.cloned() else {
            return Ok(());
        };
        let repository = match repository {
            Some(repository) => Some(daemon.repository(repository.clone()).await?.id),
            None => None,
        };
        if self.find_space(&parent).await?.is_none() && daemon.runs_elsewhere(&parent).await {
            let repository = repository.as_ref();
            return daemon
                .ensure_parent_elsewhere(None, repository, &parent)
                .await;
        }
        self.store
            .run(move |db| {
                super::ready_parent(db, repository.as_ref(), &parent)?;
                Ok(())
            })
            .await
    }

    /// One here, else one another joined machine runs, as the hub lists it.
    pub async fn parent_space_id(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        match selector {
            Some(selector) => Ok(Some(self.space_id_anywhere(daemon, selector).await?)),
            None => Ok(None),
        }
    }

    /// A child shares its parent's repository. For a parent another joined
    /// machine runs, this machine's registration of the repository the hub
    /// lists it in.
    pub async fn parent_repository(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SpaceRepository> {
        let id = self.space_id_anywhere(daemon, selector).await?;
        match self.find_space(&id).await? {
            Some(space) => Ok(match space.repository_id {
                Some(repository) => SpaceRepository::Registered(repository),
                None => SpaceRepository::Without,
            }),
            None => daemon.repository_elsewhere(&id).await,
        }
    }

    pub async fn give_to_parent(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceSelector,
        parent: Option<SpaceId>,
    ) -> Result<Workspace> {
        let id = self.workspace(workspace).await?.id;
        self.assign_parent(daemon, &id, parent).await?;
        self.workspace_by_id(&id).await
    }
}

/// An unknown parent, or a chat being removed, leaves the workspace to the
/// user. A workspace below a chat being removed stays there: the removal
/// listed it after reserving the chat, on this one thread, and takes it. A
/// parent the workspace already has stays, as one another joined machine
/// runs, which its creation recorded once the hub listed it. A new `parent`
/// the hub lists `elsewhere` takes it, and its own machine decides what it
/// takes.
fn assign(
    tx: &rusqlite::Transaction<'_>,
    workspace: &SpaceId,
    (parent, elsewhere): (Option<&SpaceId>, bool),
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
    // A chat being removed takes no new workspace, nor does a space that is
    // gone. One already below it stays there.
    let parent = match parent
        .filter(|parent| !removing.reserved(parent) || current.as_ref() == Some(*parent))
    {
        Some(parent) if elsewhere || spaces::space_kind(tx, parent)?.is_some() => Some(parent),
        _ => None,
    };
    if parent != current.as_ref() {
        match parent {
            Some(parent) => {
                let repository: RepositoryId = tx.query_row(
                    "SELECT repository_id FROM spaces WHERE id=?1",
                    [workspace],
                    |row| row.get(0),
                )?;
                super::place_below(tx, Some(&repository), workspace, parent)?;
            }
            None => super::set_parent(tx, workspace, None)?,
        }
    }
    Ok(())
}
