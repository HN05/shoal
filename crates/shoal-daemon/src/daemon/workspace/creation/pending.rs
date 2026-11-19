//! Pending worktrees: how a recorded workspace that has no worktree yet
//! gets one, which its preparation, or the creation that recorded it,
//! creates.

use super::{Configured, WorkspaceSource, stack};
use crate::daemon::restore::RestoredFiles;
use crate::daemon::workspace::ownership::CommonDir;
use crate::model::EventCause;
use crate::{
    config::modes::ModeName,
    daemon::manager::Manager,
    model::{Head, Repository, Workspace},
    protocol::CopySource,
    state::WorkspaceState,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use shoal_ids::{
    checkpoint_numbers::CheckpointNumber, repository_ids::RepositoryId, space_ids::SpaceId,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::{BranchName, CommitId};
use std::path::PathBuf;
use tokio::sync::OwnedMutexGuard;

/// How the worktree of a recorded workspace that has none yet is created,
/// as its record's `pending_plan` keeps it until it is.
#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::daemon) struct PendingWorktree {
    source: PendingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_profile: Option<String>,
    /// The mode the converted chat's agent runs in, which the settings of
    /// its new worktree must define before the workspace is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<ModeName>,
}

/// What a pending worktree checks out: a branch, as [`WorkspaceSource`]
/// names it, a commit, a copy's files, a removed workspace's checkpoint or
/// the files of a worktree outside Shoal.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingSource {
    New {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    Existing {
        branch: crate::git::existing_branch::Branch,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// Detached at `head`, as a workspace at a commit is.
    Commit { head: CommitId },
    /// A copy of the files the live or removed workspace `workspace` has,
    /// or its checkpoint `checkpoint` keeps, read when it is created.
    Copy {
        workspace: SpaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<CheckpointNumber>,
    },
    /// The restored workspace's own branch, or a detached HEAD for one at a
    /// commit, at the HEAD of its removal checkpoint `checkpoint`, which
    /// keeps the restored workspace's ID, with the unsaved files it keeps;
    /// a branch's diff compares with `base`.
    Restore {
        checkpoint: CheckpointNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// A new branch at the HEAD of the linked worktree at `path`, which
    /// Shoal does not own, with a copy of its files, as `shoal adopt
    /// --copy` makes it.
    Outside { path: PathBuf },
}

/// How a pending worktree is created: checked out on a branch or at a
/// commit, with the unsaved files a restore writes into it, or as a copy of
/// another workspace's or an outside worktree's files.
enum Creation {
    Branch(WorkspaceSource, Option<RestoredFiles>),
    Commit(CommitId, Option<RestoredFiles>),
    Copy(CopySource),
    Outside(PathBuf),
}

impl PendingWorktree {
    /// The worktree `source` creates with `git_profile`, for an agent in
    /// `mode` when it converts a chat.
    pub(super) fn of(
        source: WorkspaceSource,
        git_profile: Option<&str>,
        mode: Option<ModeName>,
    ) -> Self {
        let source = match source {
            WorkspaceSource::New { base, .. } => PendingSource::New { base },
            WorkspaceSource::Existing {
                branch,
                base,
                restores: None,
            } => PendingSource::Existing { branch, base },
            WorkspaceSource::Existing {
                base,
                restores: Some(checkpoint),
                ..
            } => PendingSource::Restore { checkpoint, base },
        };
        let git_profile = git_profile.map(str::to_owned);
        Self {
            source,
            git_profile,
            mode,
        }
    }

    /// The worktree of a workspace at a commit, detached at `head`.
    pub(in crate::daemon::workspace) fn commit(head: CommitId) -> Self {
        Self::from_source(PendingSource::Commit { head })
    }

    /// The worktree of a restored workspace at a commit, at the HEAD of its
    /// removal checkpoint `checkpoint` with the unsaved files it keeps.
    pub(in crate::daemon::workspace) fn restore(checkpoint: CheckpointNumber) -> Self {
        Self::from_source(PendingSource::Restore {
            checkpoint,
            base: None,
        })
    }

    /// A copy of the files of the linked worktree at `path`.
    pub(in crate::daemon::workspace) fn outside(path: PathBuf) -> Self {
        Self::from_source(PendingSource::Outside { path })
    }

    /// A copy of the files the workspace `workspace` has, or its
    /// `checkpoint` keeps, with `git_profile`.
    pub(in crate::daemon::workspace) fn copy(
        workspace: SpaceId,
        checkpoint: Option<CheckpointNumber>,
        git_profile: Option<&str>,
    ) -> Self {
        let source = PendingSource::Copy {
            workspace,
            checkpoint,
        };
        let git_profile = git_profile.map(str::to_owned);
        Self {
            source,
            git_profile,
            mode: None,
        }
    }

    /// The revision creating this worktree starts from or compares with,
    /// if it names one: its base, or the commit it is detached at.
    fn revision(&self) -> Option<&str> {
        match &self.source {
            PendingSource::New { base }
            | PendingSource::Existing { base, .. }
            | PendingSource::Restore { base, .. } => base.as_deref(),
            PendingSource::Commit { head } => Some(head.as_str()),
            PendingSource::Copy { .. } | PendingSource::Outside { .. } => None,
        }
    }

    fn from_source(source: PendingSource) -> Self {
        Self {
            source,
            git_profile: None,
            mode: None,
        }
    }
}

impl Manager {
    /// Create the pending worktree of the workspace `id` on `base`, the
    /// branch of the done work its agent waited for, recording the
    /// workspace that owns it as its base, as a start on `--base` does, in
    /// one transaction under the gates of any base change. One that names a
    /// base of its own or opens an existing branch keeps it.
    pub(crate) async fn build_pending_on(&self, id: &SpaceId, base: BranchName) -> Result<()> {
        let owner = self.branch_owner(id, &base).await?;
        let _gate = self.gates.lock_prs(owner.iter().chain([id])).await;
        let id = id.to_owned();
        (self.store)
            .write(move |tx| build_pending_in(tx, &id, &base, owner.as_ref()))
            .await
    }

    /// Create the worktree the recorded `workspace` is still to get, as its
    /// preparation does first, and record how creation went.
    pub(crate) async fn create_pending_worktree(
        &self,
        workspace: &SpaceSelector,
    ) -> Result<Workspace> {
        let workspace = self.workspace(workspace).await?;
        let (repo, git) = self.lock_repository(workspace.repository()).await?;
        // Read again under the Git gate, which a removal takes first.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.create_recorded_worktree(&repo, &workspace, git).await
    }

    /// Create the worktree of the recorded `workspace` of `repo` as its
    /// pending worktree says, under the repository's Git gate `git`, which
    /// settling releases, and record how creation went. One that fails
    /// leaves the workspace failed with its pending worktree, which a
    /// retried preparation creates.
    pub(in crate::daemon::workspace) async fn create_recorded_worktree(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        git: OwnedMutexGuard<()>,
    ) -> Result<Workspace> {
        let PendingWorktree {
            source,
            git_profile,
            mode,
        } = self.claim_pending_worktree(workspace).await?;
        let git_profile = git_profile.as_deref();
        let (workspace, creation) = match self.prepared_creation(repo, workspace, source).await {
            Ok(prepared) => prepared,
            Err(error) => return self.record_creation(workspace, Err(error)).await,
        };
        let workspace = &workspace;
        let result = match creation {
            Creation::Copy(copied) => {
                return (self.create_pending_copy(repo, workspace, &copied, git_profile, git))
                    .await;
            }
            Creation::Outside(path) => (self.create_outside_copy(repo, workspace, &path)).await,
            // An earlier try created it, then failed to configure it.
            Creation::Branch(_, files) | Creation::Commit(_, files)
                if workspace.checkout.git_dir().is_some() =>
            {
                (self.reconfigure_worktree(workspace, files.as_ref(), git_profile)).await
            }
            Creation::Branch(source, files) => {
                (self.materialize_worktree(repo, workspace, source, files.as_ref(), git_profile))
                    .await
            }
            Creation::Commit(head, files) => {
                (self.materialize_commit_worktree(repo, workspace, &head, files.as_ref())).await
            }
        };
        let result = self.with_mode(workspace, mode.as_ref(), result).await;
        self.settle_creation(repo, workspace, result, git).await
    }

    /// The `configured` worktree of `workspace` once its settings define the
    /// `mode` of a converted chat's agent, which a workspace that is not
    /// ready yet refuses before anything uses it.
    async fn with_mode(
        &self,
        workspace: &Workspace,
        mode: Option<&ModeName>,
        configured: Result<Configured>,
    ) -> Result<Configured> {
        match (configured, mode) {
            (Ok(configured), Some(mode)) => {
                (self.ensure_worktree_mode(workspace, mode).await).map(|()| configured)
            }
            (configured, _) => configured,
        }
    }

    /// The `workspace` of `repo` with the admin directory of the worktree an
    /// earlier try left recorded, once what an add it cut short left is
    /// gone, and how its worktree is created from `source`.
    async fn prepared_creation(
        &self,
        repo: &Repository,
        workspace: &Workspace,
        source: PendingSource,
    ) -> Result<(Workspace, Creation)> {
        let common_dir = CommonDir::of(&repo.path);
        self.discard_unfinished_add(workspace, &common_dir).await?;
        let workspace = self.recover_git_dir(workspace, &common_dir).await?;
        let creation = self.creation(&workspace, source).await?;
        Ok((workspace, creation))
    }

    /// How the worktree of `workspace` that `source` describes is created,
    /// reading a restore's checkpoint again.
    async fn creation(&self, workspace: &Workspace, source: PendingSource) -> Result<Creation> {
        Ok(match source {
            PendingSource::New { base } => {
                Creation::Branch(WorkspaceSource::New { base, parent: None }, None)
            }
            PendingSource::Existing { branch, base } => Creation::Branch(
                WorkspaceSource::Existing {
                    branch,
                    base,
                    restores: None,
                },
                None,
            ),
            PendingSource::Commit { head } => Creation::Commit(head, None),
            PendingSource::Copy {
                workspace,
                checkpoint,
            } => Creation::Copy(CopySource {
                workspace: SpaceSelector::Id(workspace),
                checkpoint,
            }),
            PendingSource::Restore { checkpoint, base } => {
                self.restore_creation(workspace, checkpoint, base).await?
            }
            PendingSource::Outside { path } => Creation::Outside(path),
        })
    }

    /// How the restored `workspace` is created at the HEAD of its removal
    /// checkpoint `number`, with the unsaved files it keeps: on its own
    /// branch, which its restore readied, with its diff comparing with
    /// `base`, or detached.
    async fn restore_creation(
        &self,
        workspace: &Workspace,
        number: CheckpointNumber,
        base: Option<String>,
    ) -> Result<Creation> {
        let id = workspace.id.clone();
        let checkpoint = (self.store)
            .run(move |db| super::super::checkpoints::find(db, &id, number))
            .await?
            .with_context(|| {
                format!(
                    "checkpoint {number} of {}, which its restore opens, is gone",
                    workspace.name
                )
            })?;
        let files = RestoredFiles::of(&checkpoint);
        Ok(match &workspace.head {
            Head::Branch(name) => {
                let branch = crate::git::existing_branch::Branch {
                    name: name.clone(),
                    remote: None,
                };
                let source = WorkspaceSource::Existing {
                    branch,
                    base,
                    restores: None,
                };
                Creation::Branch(source, files)
            }
            Head::Commit(_) => Creation::Commit(checkpoint.head, files),
        })
    }

    /// Configure the worktree an earlier creation of `workspace` left once
    /// it verifies as the one Git created for it, still on its recorded
    /// branch or detached, writing a restore's `files` into it again once a
    /// checkpoint keeps what it holds.
    async fn reconfigure_worktree(
        &self,
        workspace: &Workspace,
        files: Option<&RestoredFiles>,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        match files {
            Some(_) => self.keep_before_retry(workspace).await?,
            None => {
                let branch = workspace.branch().map(shoal_refs::BranchName::as_str);
                self.verify_checkout(workspace, branch).await?
            }
        }
        self.configure_worktree(workspace, files, git_profile).await
    }

    /// Verify the worktree an earlier creation of `workspace` left as the
    /// one Git created for it, still on its recorded branch or detached,
    /// and keep what it holds in a checkpoint before a retry writes files
    /// into it again, refusing a Git operation under way there and what a
    /// checkpoint cannot keep.
    pub(in crate::daemon::workspace) async fn keep_before_retry(
        &self,
        workspace: &Workspace,
    ) -> Result<()> {
        let branch = workspace.branch().map(shoal_refs::BranchName::as_str);
        self.verify_checkout(workspace, branch).await?;
        ensure!(
            !crate::git::status::operation_in_progress(&workspace.path).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        let kept = self
            .checkpoint(workspace, crate::model::CheckpointCause::Refresh, None)
            .await?;
        ensure!(
            !kept.files_omitted,
            "checkpoint {} could not keep {}'s unsaved files; commit or remove them first",
            kept.number,
            workspace.name
        );
        Ok(())
    }

    /// The pending worktree of `workspace`, which goes back to preparing
    /// for its creation, refusing a workspace that has its worktree or
    /// that another operation, such as a removal, holds.
    async fn claim_pending_worktree(&self, workspace: &Workspace) -> Result<PendingWorktree> {
        let (id, name) = (workspace.id.clone(), workspace.name.clone());
        let plan = self
            .store
            .write(move |tx| {
                let plan: Option<String> = tx.query_row(
                    "SELECT pending_plan FROM workspaces WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )?;
                let Some(plan) = plan else {
                    bail!("{name} already has its worktree");
                };
                let claimed = tx.execute(
                    "UPDATE workspaces SET state=?2,error=NULL WHERE id=?1 AND state IN (?2,?3)",
                    params![id, WorkspaceState::Preparing, WorkspaceState::Failed],
                )?;
                ensure!(claimed == 1, "another operation holds {name}");
                Ok(plan)
            })
            .await?;
        Ok(serde_json::from_str(&plan)?)
    }
}

/// The revisions the pending worktrees of `repository`'s workspaces start
/// from or compare with, as their plans name them.
pub(crate) fn pending_revisions(
    tx: &rusqlite::Transaction<'_>,
    repository: &RepositoryId,
) -> Result<Vec<String>> {
    let mut plans = tx.prepare(
        "SELECT pending_plan FROM spaces JOIN workspaces USING(id)
         WHERE repository_id=?1 AND pending_plan IS NOT NULL",
    )?;
    let plans = plans.query_map([repository], |row| row.get::<_, String>(0))?;
    let mut revisions = Vec::new();
    for plan in plans {
        let plan: PendingWorktree = serde_json::from_str(&plan?)?;
        revisions.extend(plan.revision().map(str::to_owned));
    }
    Ok(revisions)
}

/// [`Manager::build_pending_on`] in `tx`, with `owner` the workspace that
/// owns `base`, if any, whose PR gate and that of `id` the caller holds.
/// An owner that came to build on the workspace `id` meanwhile is no base
/// of it, which would make a cycle, though the worktree still holds its
/// work.
pub(crate) fn build_pending_in(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    base: &BranchName,
    owner: Option<&SpaceId>,
) -> Result<()> {
    let plan: Option<String> = tx.query_row(
        "SELECT pending_plan FROM workspaces WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    let plan = plan.context("the workspace already has its worktree")?;
    let mut pending: PendingWorktree = serde_json::from_str(&plan)?;
    let PendingSource::New { base: unset @ None } = &mut pending.source else {
        return Ok(());
    };
    *unset = Some(base.to_string());
    tx.execute(
        "UPDATE workspaces SET pending_plan=?2 WHERE id=?1",
        params![id, serde_json::to_string(&pending)?],
    )?;
    match owner {
        Some(owner) if stack::builds_on(tx, owner, id)? => Ok(()),
        Some(owner) => stack::stack_on(tx, id, owner, EventCause::Blocker),
        None => Ok(()),
    }
}
