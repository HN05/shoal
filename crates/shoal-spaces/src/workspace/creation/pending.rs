//! Pending worktrees: how a recorded workspace without a worktree yet gets
//! one, which its preparation or the creation that recorded it creates.

use super::{Configured, WorkspaceSource, stack};
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use crate::restore::RestoredFiles;
use crate::workspace::ownership::CommonDir;
use crate::{
    config::modes::ModeName,
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

/// The record's `pending_plan` keeps it until the worktree exists.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PendingWorktree {
    source: PendingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_profile: Option<String>,
    /// The mode the converted chat's agent runs in. The settings of the new
    /// worktree must define it before the workspace is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<ModeName>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingSource {
    /// `files` keeps the unsaved files the branch starts with, as a copy
    /// another joined machine carried here does.
    New {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<CommitId>,
    },
    Existing {
        branch: crate::git::existing_branch::Branch,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// Detached at `head`, as a workspace at a commit is.
    Commit { head: CommitId },
    /// The files are read when the worktree is created.
    Copy {
        workspace: SpaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<CheckpointNumber>,
    },
    /// The restored workspace's own branch, or a detached HEAD for one at a
    /// commit, at the HEAD of its removal checkpoint, with the unsaved files
    /// the checkpoint keeps. The checkpoint keeps the restored workspace's
    /// ID. A branch's diff compares with `base`.
    Restore {
        checkpoint: CheckpointNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
    /// A new branch at the HEAD of a linked worktree Shoal does not own,
    /// with a copy of its files, as `shoal adopt --copy` makes it.
    Outside { path: PathBuf },
}

enum Creation {
    Branch(WorkspaceSource, Option<RestoredFiles>),
    Commit(CommitId, Option<RestoredFiles>),
    Copy(CopySource),
    Outside(PathBuf),
}

impl PendingWorktree {
    /// `files` keeps the unsaved files a new branch starts with. `mode` is
    /// set when the worktree converts a chat.
    pub(super) fn of(
        (source, files): (WorkspaceSource, Option<CommitId>),
        git_profile: Option<&str>,
        mode: Option<ModeName>,
    ) -> Self {
        let source = match source {
            WorkspaceSource::New { base, .. } => PendingSource::New { base, files },
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

    pub(in crate::workspace) fn commit(head: CommitId) -> Self {
        Self::from_source(PendingSource::Commit { head })
    }

    /// The worktree of a restored workspace at a commit, at the HEAD of its
    /// removal checkpoint, with the unsaved files it keeps.
    pub(in crate::workspace) fn restore(checkpoint: CheckpointNumber) -> Self {
        Self::from_source(PendingSource::Restore {
            checkpoint,
            base: None,
        })
    }

    pub(in crate::workspace) fn outside(path: PathBuf) -> Self {
        Self::from_source(PendingSource::Outside { path })
    }

    pub(in crate::workspace) fn copy(
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

    /// Its base, or the commit it is detached at, and the commit that keeps
    /// the unsaved files it starts with.
    fn revisions(&self) -> Vec<&str> {
        match &self.source {
            PendingSource::New { base, files } => {
                let files = files.as_ref().map(CommitId::as_str);
                base.as_deref().into_iter().chain(files).collect()
            }
            PendingSource::Existing { base, .. } | PendingSource::Restore { base, .. } => {
                base.as_deref().into_iter().collect()
            }
            PendingSource::Commit { head } => vec![head.as_str()],
            PendingSource::Copy { .. } | PendingSource::Outside { .. } => Vec::new(),
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

impl Spaces {
    /// `base` is the branch of the done work the agent waited for. Records
    /// the workspace that owns it as the base, as a start on `--base` does,
    /// in one transaction under the gates of any base change. A workspace
    /// that names a base of its own or opens an existing branch keeps it.
    pub async fn build_pending_on(&self, id: &SpaceId, base: BranchName) -> Result<()> {
        let owner = self.branch_owner(id, &base).await?;
        let _gate = self.gates.lock_prs(owner.iter().chain([id])).await;
        let id = id.to_owned();
        (self.store)
            .write(move |tx| build_pending_in(tx, &id, &base, owner.as_ref()))
            .await
    }

    /// Records how creation went.
    pub async fn create_pending_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &SpaceSelector,
    ) -> Result<Workspace> {
        let workspace = self.workspace(workspace).await?;
        let (repo, git) = daemon
            .lock_repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        // Read again under the Git gate, which a removal takes first.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        self.create_recorded_worktree(daemon, &repo, &workspace, git)
            .await
    }

    /// Runs under the repository's Git gate `git`, which settling releases.
    /// A failure leaves the workspace failed with its pending worktree,
    /// which a retried preparation creates.
    pub(in crate::workspace) async fn create_recorded_worktree(
        &self,
        daemon: &dyn Daemon,
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
                return (self.create_pending_copy(
                    daemon,
                    repo,
                    workspace,
                    &copied,
                    git_profile,
                    git,
                ))
                .await;
            }
            Creation::Outside(path) => {
                (self.create_outside_copy(daemon, repo, workspace, &path)).await
            }
            // An earlier try created it, then failed to configure it.
            Creation::Branch(_, files) | Creation::Commit(_, files)
                if workspace.checkout.git_dir().is_some() =>
            {
                (self.reconfigure_worktree(daemon, workspace, files.as_ref(), git_profile)).await
            }
            Creation::Branch(source, files) => {
                (self.materialize_worktree(
                    daemon,
                    repo,
                    workspace,
                    source,
                    files.as_ref(),
                    git_profile,
                ))
                .await
            }
            Creation::Commit(head, files) => {
                (self.materialize_commit_worktree(daemon, repo, workspace, &head, files.as_ref()))
                    .await
            }
        };
        let result = self
            .with_mode(daemon, workspace, mode.as_ref(), result)
            .await;
        self.settle_creation(daemon, repo, workspace, result, git)
            .await
    }

    /// Refuses, before anything uses the worktree, settings that do not
    /// define the `mode` of a converted chat's agent.
    async fn with_mode(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        mode: Option<&ModeName>,
        configured: Result<Configured>,
    ) -> Result<Configured> {
        match (configured, mode) {
            (Ok(configured), Some(mode)) => {
                (self.ensure_worktree_mode(daemon, workspace, mode).await).map(|()| configured)
            }
            (configured, _) => configured,
        }
    }

    /// Records the admin directory an earlier try left, once what a cut-short
    /// add left is gone.
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

    /// Reads a restore's checkpoint again.
    async fn creation(&self, workspace: &Workspace, source: PendingSource) -> Result<Creation> {
        Ok(match source {
            PendingSource::New { base, files } => {
                let files = carried_files(base.as_deref(), files)?;
                Creation::Branch(WorkspaceSource::New { base, parent: None }, files)
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

    /// On its own branch, which its restore readied, with its diff comparing
    /// with `base`, or detached.
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

    /// Configures it once it verifies as the one Git created for it, still
    /// on its recorded branch or detached. Writes a restore's `files` into
    /// it again once a checkpoint keeps what it holds.
    async fn reconfigure_worktree(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        files: Option<&RestoredFiles>,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        match files {
            Some(_) => self.keep_before_retry(daemon, workspace).await?,
            None => {
                let branch = workspace.branch().map(shoal_refs::BranchName::as_str);
                self.verify_checkout(daemon, workspace, branch).await?
            }
        }
        self.configure_worktree(daemon, workspace, files, git_profile)
            .await
    }

    /// Verifies it as the one Git created for it, still on its recorded
    /// branch or detached. Refuses a Git operation under way there and what
    /// a checkpoint cannot keep.
    pub(in crate::workspace) async fn keep_before_retry(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<()> {
        let branch = workspace.branch().map(shoal_refs::BranchName::as_str);
        self.verify_checkout(daemon, workspace, branch).await?;
        ensure!(
            !crate::git::status::operation_in_progress(&workspace.path).await?,
            "a Git operation is under way in workspace {}; finish or abort it first",
            workspace.name
        );
        let kept = self
            .checkpoint(
                daemon,
                workspace,
                crate::model::CheckpointCause::Refresh,
                None,
            )
            .await?;
        ensure!(
            !kept.files_omitted,
            "checkpoint {} could not keep {}'s unsaved files; commit or remove them first",
            kept.number,
            workspace.name
        );
        Ok(())
    }

    /// The workspace goes back to preparing for the creation. Refuses one
    /// that has its worktree or that another operation, such as a removal,
    /// holds.
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

/// As their plans name them.
pub fn pending_revisions(
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
        revisions.extend(plan.revisions().into_iter().map(str::to_owned));
    }
    Ok(revisions)
}

/// [`crate::Spaces::build_pending_on`] in `tx`. `owner` is the workspace
/// that owns `base`, if any. The caller holds its PR gate and that of `id`.
/// An owner that came to build on the workspace `id` meanwhile is not made
/// its base, which would make a cycle, though the worktree still holds its
/// work.
pub fn build_pending_in(
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
    let PendingSource::New {
        base: unset @ None, ..
    } = &mut pending.source
    else {
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

/// None where `files` is `base` itself. Refuses a commit without a base.
fn carried_files(base: Option<&str>, files: Option<CommitId>) -> Result<Option<RestoredFiles>> {
    let Some(snapshot) = files.filter(|files| base != Some(files.as_str())) else {
        return Ok(None);
    };
    let base = base.context("a new branch that starts with carried files names its base")?;
    Ok(Some(RestoredFiles {
        head: CommitId::from(base),
        snapshot,
    }))
}
