//! The tree of spaces, which `space_parents` records. See
//! `docs/design/spaces.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension};

use crate::{
    model::{Repository, Space, SpaceRef, Workspace},
    protocol::CopySource,
    state::WorkspaceState,
};
use reads::{TAKEN_FIRST, taken};
pub use reads::{
    agent_runs, below_elsewhere, children, delegated, delegated_elsewhere, parent, parent_id,
    parent_ids, receiver_from, receiving_paths, removal_depth, subtree_query, with_agents,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_store::tree::space_ref;
pub use shoal_store::tree::{Receiver, TrackedAgents, coordinator_of, first_absent_above, subtree};

impl Spaces {
    /// Leaves out what `options` say.
    pub async fn create_child(
        &self,
        daemon: &dyn Daemon,
        parent: &SpaceSelector,
        name: String,
        base: Option<String>,
        options: crate::protocol::CreationOptions,
    ) -> Result<Workspace> {
        let (parent, repo) = self.child_parent(daemon, parent, None).await?;
        let (repo, git) = daemon
            .lock_repository(crate::repository::Selector::from(&repo.id))
            .await?;
        crate::git::check_branch_name(&name)?;
        let branch = self.available_branch(&repo, &name).await?;
        self.create_branch_workspace(
            daemon,
            &repo,
            name,
            branch,
            super::workspace::creation::WorkspaceSource::New {
                base,
                parent: Some(parent.id),
            },
            super::workspace::NewWorkspace {
                options,
                ..Default::default()
            },
            git,
        )
        .await
    }

    /// Returns the repository the child goes in: the parent's own, or for a
    /// chat without one that of the workspace `copy` copies. A child on a
    /// branch of such a chat names none.
    pub async fn child_parent(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        copy: Option<&CopySource>,
    ) -> Result<(Space, Repository)> {
        let parent = self.space(selector).await?;
        let repository = match (&parent.repository_id, copy) {
            (Some(own), _) => own.clone(),
            (None, Some(copy)) => self.copied_repository(copy).await?,
            (None, None) => bail!(
                "{} belongs to no repository to create a child workspace in; create it below \
                 a space of the child's repository, or copy a workspace with --copy",
                parent.label()
            ),
        };
        let repository = daemon
            .repository(crate::repository::Selector::from(repository))
            .await?;
        Ok((parent, repository))
    }

    pub async fn within_subtree(&self, ancestor: &SpaceId, id: &SpaceId) -> Result<bool> {
        let (ancestor, id) = (ancestor.to_owned(), id.to_owned());
        self.store
            .run(move |db| Ok(subtree(db, &ancestor)?.contains(&id)))
            .await
    }

    /// `id` first.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn subtree(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        let id = id.to_owned();
        self.store.run(move |db| subtree(db, &id)).await
    }

    /// Nearest first: the spaces below it and the chats in each workspace's
    /// worktree among them.
    pub async fn spaces_taken_with(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "{} SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM taken
                            JOIN spaces USING(id) LEFT JOIN workspaces ON workspaces.id=spaces.id
                            WHERE depth > 0 ORDER BY depth, spaces.id",
                        taken()
                    ))?
                    .query_map([id], space_ref)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Each comes before those below it: the chats in its worktree, then the
    /// spaces directly below it.
    pub async fn taken_first(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch
                            FROM {TAKEN_FIRST} AS first
                            JOIN spaces ON spaces.id=first.space_id
                            LEFT JOIN workspaces ON workspaces.id=spaces.id
                            WHERE first.parent_id=?1 ORDER BY spaces.kind, spaces.name"
                    ))?
                    .query_map([id], space_ref)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Nearest first.
    pub async fn ancestors(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store
            .run(move |db| shoal_store::tree::ancestors(db, &id))
            .await
    }

    /// By name.
    pub async fn children(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        self.store.run(move |db| children(db, &id)).await
    }

    /// Counts spaces on other machines too.
    pub async fn has_spaces_below(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<bool> {
        if !self.children(id).await?.is_empty() {
            return Ok(true);
        }
        Ok(!daemon.descendants_elsewhere(id).await?.is_empty())
    }

    /// Children without an agent of their own, and theirs in turn.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn delegated_spaces(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<SpaceRef>> {
        let id = id.to_owned();
        let agents = self.tracked_agents(daemon).await;
        self.store.run(move |db| delegated(db, &id, &agents)).await
    }

    pub async fn tracked_agents(&self, daemon: &dyn Daemon) -> TrackedAgents {
        TrackedAgents::of(&daemon.running_agent_workspaces().await)
    }
}

/// The parent must be a ready workspace or a chat of the same repository, or
/// a chat without a repository. It may be neither below `space` nor in the
/// worktree of a workspace there, since removal takes both first. Removal
/// reserves the parent first and rechecks its children afterwards, so a
/// child either precedes that recheck or is refused here.
pub fn ensure_parent(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    space: &SpaceId,
    parent: &SpaceId,
) -> Result<()> {
    let name = ready_parent(db, repository_id, parent)?;
    let taken: bool = db.query_row(
        &format!("{} SELECT EXISTS(SELECT 1 FROM taken WHERE id=?2)", taken()),
        [space, parent],
        |row| row.get(0),
    )?;
    ensure!(
        !taken,
        "{name} is below or in the space it would take as a child"
    );
    Ok(())
}

/// Refuses a workspace that is not ready.
pub fn ready_parent(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    parent: &SpaceId,
) -> Result<String> {
    let (name, state) = parent_in_repository(db, repository_id, parent)?;
    if let Some(state) = state {
        ensure!(
            state == WorkspaceState::Ready,
            "parent workspace is {state}; open a child below a ready workspace"
        );
    }
    Ok(name)
}

/// A chat without a repository takes children in any repository.
pub fn parent_in_repository(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    parent: &SpaceId,
) -> Result<(String, Option<WorkspaceState>)> {
    let recorded: Option<(Option<RepositoryId>, String, Option<WorkspaceState>)> = db
        .query_row(
            "SELECT spaces.repository_id,spaces.name,workspaces.state FROM spaces
                LEFT JOIN workspaces USING(id) WHERE spaces.id=?1",
            [parent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((repository, name, state)) = recorded else {
        bail!("parent space {parent} no longer exists");
    };
    ensure!(
        repository.is_none() || repository.as_ref() == repository_id,
        "{name} is in another repository, and only a chat without a repository takes \
         children in other repositories"
    );
    Ok((name, state))
}

/// Makes the space top-level when `parent` is `None`.
pub fn set_parent(db: &Connection, space: &SpaceId, parent: Option<&SpaceId>) -> Result<()> {
    match parent {
        Some(parent) => db.execute(
            "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)
                ON CONFLICT(space_id) DO UPDATE SET parent_id=excluded.parent_id",
            [space, parent],
        )?,
        None => db.execute("DELETE FROM space_parents WHERE space_id=?1", [space])?,
    };
    Ok(())
}

/// Call once [`ensure_parent`] finds that the parent takes the space. A
/// parent this machine does not have runs on another joined machine, as a
/// restore found, and that machine decides what it takes.
pub fn place_below(
    db: &Connection,
    repository_id: Option<&RepositoryId>,
    space: &SpaceId,
    parent: &SpaceId,
) -> Result<()> {
    let here = crate::store::exists(db, "SELECT 1 FROM spaces WHERE id=?1", [parent])?;
    if here {
        ensure_parent(db, repository_id, space, parent)?;
    }
    set_parent(db, space, Some(parent))
}

pub fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.parent = parent(db, &workspace.id)?;
    workspace.children = children(db, &workspace.id)?;
    Ok(())
}

pub mod move_up;
mod parents;
mod reads;

#[cfg(test)]
mod tests;
