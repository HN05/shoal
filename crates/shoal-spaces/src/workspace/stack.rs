//! Base workspaces: the workspace whose branch a stacked workspace builds
//! on.
use crate::Spaces;
use crate::model::EventCause;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;
use std::path::Path;

use crate::refusal::{Reason, Refusal};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::state::ConversionTarget;

use crate::{
    git,
    model::{Workspace, WorkspaceRef},
    {events::SpaceEvent, store},
};

impl Spaces {
    /// Whatever base workspace the other links.
    pub async fn branch_built_on(&self, workspace: &Workspace) -> Result<bool> {
        let Some(branch) = workspace.branch() else {
            return Ok(false);
        };
        let reference = git::local_ref(branch);
        let (id, repository) = (workspace.id.clone(), workspace.repository().clone());
        self.store
            .read(move |tx| {
                store::exists(
                    tx,
                    "SELECT 1 FROM workspaces JOIN spaces USING(id)
                        WHERE workspaces.base_ref=?1 AND id<>?2 AND spaces.repository_id=?3",
                    params![reference, id, repository],
                )
            })
            .await
    }

    pub async fn branch_owner(&self, id: &SpaceId, branch: &BranchName) -> Result<Option<SpaceId>> {
        let repository_id = self.workspace_by_id(id).await?.repository().clone();
        self.branch_owner_in(&repository_id, branch).await
    }

    pub async fn branch_owner_in(
        &self,
        repository_id: &RepositoryId,
        branch: &BranchName,
    ) -> Result<Option<SpaceId>> {
        let (repository_id, branch) = (repository_id.clone(), branch.clone());
        (self.store)
            .read(move |tx| owner(tx, &repository_id, &branch))
            .await
    }

    /// Fails when it would not change.
    pub async fn set_base_workspace(
        &self,
        selector: &SpaceSelector,
        base: Option<String>,
    ) -> Result<Workspace> {
        let workspace = self.workspace(selector).await?;
        if base.is_some() {
            workspace.require_branch("stacking on a base workspace")?;
        }
        let repository_id = workspace.repository().clone();
        let base = match base {
            Some(base) => Some(
                self.store
                    .run(move |db| base_workspace(db, &repository_id, &base))
                    .await?,
            ),
            None => None,
        };
        // The PR sweep restacks under the gates of a base and the workspaces
        // stacked on it, so a change cannot land between its retarget and
        // its recorded move, nor stack a workspace on a base it is removing.
        let _gate = self
            .gates
            .lock_prs(base.iter().chain([&workspace.id]))
            .await;
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let (id, repository_id) = (workspace.id.clone(), workspace.repository().clone());
        self.store
            .write(move |tx| change_base(tx, &repository_id, &id, base.as_ref()))
            .await?;
        self.workspace_by_id(&workspace.id).await
    }
}

/// Journals the change. Fails when nothing would change.
fn change_base(
    tx: &rusqlite::Transaction<'_>,
    repository_id: &RepositoryId,
    id: &SpaceId,
    base: Option<&SpaceId>,
) -> Result<()> {
    if let Some(base) = base {
        ensure_base(tx, repository_id, base, base.as_str())?;
        ensure_acyclic(tx, id, base)?;
    }
    let changed = tx.execute(
        "UPDATE workspaces SET base_workspace_id=?2 WHERE id=?1 AND base_workspace_id IS NOT ?2",
        params![id, base],
    )?;
    match &base {
        Some(_) => ensure!(changed > 0, "the workspace already builds on that base"),
        None => ensure!(changed > 0, "the workspace has no base workspace"),
    }
    record_base_changed(tx, id, EventCause::Manual)?;
    Ok(())
}

fn ensure_acyclic(db: &Connection, id: &SpaceId, base: &SpaceId) -> Result<()> {
    ensure!(
        !builds_on(db, base, id)?,
        "a workspace cannot build on itself or a workspace stacked on it"
    );
    Ok(())
}

/// Directly or through the workspaces stacked between.
pub fn builds_on(db: &Connection, id: &SpaceId, base: &SpaceId) -> Result<bool> {
    store::exists(
        db,
        "WITH RECURSIVE chain(id) AS (
            SELECT ?1 UNION SELECT base_workspace_id FROM workspaces
            JOIN chain USING(id) WHERE base_workspace_id IS NOT NULL
        ) SELECT 1 FROM chain WHERE id=?2",
        [id, base],
    )
}

/// Refuses a base that would make the workspace build on itself. The
/// caller holds the PR gates of both, as every base change takes them.
pub fn stack_on(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    base: &SpaceId,
    cause: EventCause,
) -> Result<()> {
    ensure_acyclic(tx, id, base)?;
    tx.execute(
        "UPDATE workspaces SET base_workspace_id=?2 WHERE id=?1",
        params![id, base],
    )?;
    record_base_changed(tx, id, cause)
}

pub fn record_base_changed(db: &Connection, id: &SpaceId, cause: EventCause) -> Result<()> {
    let base_workspace = store::journal::base_of(db, id)?;
    let event = SpaceEvent::BaseChanged { base_workspace };
    store::journal::append_caused(db, id, event, Some(cause), None)
}

/// By number or ID, else by branch.
fn base_workspace(
    db: &Connection,
    repository_id: &RepositoryId,
    selector: &str,
) -> Result<SpaceId> {
    let selected = match SpaceSelector::parse(selector) {
        Ok(selector) => crate::spaces::selected(db, &selector)?,
        Err(_) => None,
    };
    if let Some((crate::model::SpaceKind::Workspace, id)) = selected {
        ensure_base(db, repository_id, &id, selector)?;
        return Ok(id);
    }
    Ok(owner(db, repository_id, &selector.into())?
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown workspace: {selector}")))?)
}

/// `shown` is how the refusal names it.
fn ensure_base(
    db: &Connection,
    repository_id: &RepositoryId,
    id: &SpaceId,
    shown: &str,
) -> Result<()> {
    let repository = db
        .query_row(
            "SELECT repository_id FROM spaces WHERE id=?1 AND kind='workspace'",
            [id],
            |row| row.get::<_, RepositoryId>(0),
        )
        .optional()?
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown workspace: {shown}")))?;
    ensure!(
        repository == *repository_id,
        "base workspace {shown} belongs to another repository"
    );
    ensure!(
        store::exists(
            db,
            "SELECT 1 FROM workspaces WHERE id=?1 AND branch IS NOT NULL",
            [id]
        )?,
        "base workspace {shown} is at a commit and has no branch to build on"
    );
    ensure!(
        !converting_to_chat(db, id)?,
        "base workspace {shown} becomes a chat, whose worktree goes; build on its branch without it"
    );
    Ok(())
}

pub(super) fn load(db: &Connection, workspace: &mut Workspace) -> Result<()> {
    workspace.base_workspace = db
        .prepare_cached(
            "SELECT base.id,space.name,base.branch FROM workspaces stacked
             JOIN workspaces base ON base.id=stacked.base_workspace_id
             JOIN spaces space ON space.id=base.id WHERE stacked.id=?1",
        )?
        .query_row([&workspace.id], row)
        .optional()?;
    workspace.stacked_workspaces = db
        .prepare_cached(
            "SELECT id,name,branch FROM spaces JOIN workspaces USING(id)
             WHERE base_workspace_id=?1 ORDER BY name",
        )?
        .query_map([&workspace.id], row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(())
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRef> {
    Ok(WorkspaceRef {
        id: row.get(0)?,
        name: row.get(1)?,
        branch: row.get(2)?,
    })
}

/// The branch may be local or on a remote, and another workspace may own
/// it. An unresolvable base infers nothing; creation reports it.
pub(super) async fn base_branch(repo: &Path, base: &str) -> Result<Option<BranchName>> {
    if let Some((_, branch)) = git::remote_branch(repo, base).await? {
        return Ok(Some(branch));
    }
    let Ok(reference) = git::symbolic_revision(repo, base, git::run_isolated).await else {
        return Ok(None);
    };
    Ok(git::strip_local(reference.trim_end()).map(BranchName::from))
}

/// A workspace that becomes a chat is not one a new workspace may build on.
pub(super) fn owner(
    db: &Connection,
    repository_id: &RepositoryId,
    branch: &BranchName,
) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            "SELECT id FROM spaces JOIN workspaces USING(id) WHERE repository_id=?1 AND branch=?2
                AND id NOT IN (SELECT space_id FROM space_conversions WHERE target=?3)",
            params![repository_id, branch, ConversionTarget::Chat],
            |row| row.get(0),
        )
        .optional()?)
}

/// Once it does, nothing may build on it, as its worktree goes then.
fn converting_to_chat(db: &Connection, id: &SpaceId) -> Result<bool> {
    store::exists(
        db,
        "SELECT 1 FROM space_conversions WHERE space_id=?1 AND target=?2",
        params![id, ConversionTarget::Chat],
    )
}

#[cfg(test)]
mod tests;
