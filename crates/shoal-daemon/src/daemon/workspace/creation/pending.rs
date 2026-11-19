//! Pending worktrees: how a recorded workspace that has no worktree yet
//! gets one, which its preparation creates.

use super::{Configured, WorkspaceSource, stack};
use crate::model::EventCause;
use crate::{
    daemon::workspace::Manager, model::Workspace, protocol::CopySource, state::WorkspaceState,
};
use anyhow::{Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use shoal_ids::{checkpoint_numbers::CheckpointNumber, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::{BranchName, CommitId};

/// How the worktree of a recorded workspace that has none yet is created,
/// as `pending_worktrees` keeps it until it is.
#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::daemon::workspace) struct PendingWorktree {
    source: PendingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_profile: Option<String>,
}

/// What a pending worktree checks out: a branch, as [`WorkspaceSource`]
/// names it without what only a restore writes into it, a commit, or a
/// copy's files.
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
}

/// How a pending worktree is created: checked out on a branch or at a
/// commit, or as a copy of another workspace's files.
enum Creation {
    Branch(WorkspaceSource),
    Commit(CommitId),
    Copy(CopySource),
}

impl PendingSource {
    /// How the worktree it checks out is created.
    fn creation(self) -> Creation {
        match self {
            Self::New { base } => Creation::Branch(WorkspaceSource::New { base, parent: None }),
            Self::Existing { branch, base } => Creation::Branch(WorkspaceSource::Existing {
                branch,
                base,
                files: None,
            }),
            Self::Commit { head } => Creation::Commit(head),
            Self::Copy {
                workspace,
                checkpoint,
            } => Creation::Copy(CopySource {
                workspace: SpaceSelector::Id(workspace),
                checkpoint,
            }),
        }
    }
}

impl PendingWorktree {
    /// The worktree `source` creates with `git_profile`, refusing a
    /// restore's files, which go in before anything can use the workspace.
    pub(super) fn of(source: WorkspaceSource, git_profile: Option<&str>) -> Result<Self> {
        let source = match source {
            WorkspaceSource::New { base, .. } => PendingSource::New { base },
            WorkspaceSource::Existing {
                branch,
                base,
                files: None,
            } => PendingSource::Existing { branch, base },
            WorkspaceSource::Existing { files: Some(_), .. } => {
                bail!("a restore's worktree is created with its record")
            }
        };
        let git_profile = git_profile.map(str::to_owned);
        Ok(Self {
            source,
            git_profile,
        })
    }

    /// The worktree of a workspace at a commit, detached at `head`.
    pub(in crate::daemon::workspace) fn commit(head: CommitId) -> Self {
        Self {
            source: PendingSource::Commit { head },
            git_profile: None,
        }
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
    /// preparation does first, and record how creation went as a creation
    /// with its record does. One that fails leaves the workspace failed
    /// with its pending worktree, which a retried preparation creates.
    pub(crate) async fn create_pending_worktree(
        &self,
        workspace: &SpaceSelector,
    ) -> Result<Workspace> {
        let workspace = self.workspace(workspace).await?;
        let (repo, git) = self.lock_repository(workspace.repository()).await?;
        // Read again under the Git gate, which a removal takes first.
        let workspace = self.workspace_by_id(&workspace.id).await?;
        let PendingWorktree {
            source,
            git_profile,
        } = self.claim_pending_worktree(&workspace).await?;
        let git_profile = git_profile.as_deref();
        let result = match source.creation() {
            Creation::Copy(copied) => {
                return (self.create_pending_copy(&repo, &workspace, &copied, git_profile, git))
                    .await;
            }
            // An earlier try created it, then failed to configure it.
            _ if workspace.git_dir.is_some() => {
                self.reconfigure_worktree(&workspace, git_profile).await
            }
            Creation::Branch(source) => {
                (self.materialize_worktree(&repo, &workspace, source, git_profile)).await
            }
            Creation::Commit(head) => {
                (self.materialize_commit_worktree(&repo, &workspace, &head, None)).await
            }
        };
        self.settle_creation(&repo, &workspace, result, git).await
    }

    /// Configure the worktree an earlier creation of `workspace` left once
    /// it verifies as the one Git created for it, still on its recorded
    /// branch or detached.
    async fn reconfigure_worktree(
        &self,
        workspace: &Workspace,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        let branch = workspace.branch().map(shoal_refs::BranchName::as_str);
        self.verify_checkout(workspace, branch).await?;
        self.configure_worktree(workspace, None, git_profile).await
    }

    /// The pending worktree of `workspace`, which goes back to preparing
    /// for its creation, refusing a workspace that has its worktree or
    /// that another operation, such as a removal, holds.
    async fn claim_pending_worktree(&self, workspace: &Workspace) -> Result<PendingWorktree> {
        let (id, name) = (workspace.id.clone(), workspace.name.clone());
        let plan = self
            .store
            .write(move |tx| {
                let plan: Option<String> = tx
                    .query_row(
                        "SELECT plan FROM pending_worktrees WHERE workspace_id=?1",
                        [&id],
                        |row| row.get(0),
                    )
                    .optional()?;
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
    let plan: String = tx.query_row(
        "SELECT plan FROM pending_worktrees WHERE workspace_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let mut pending: PendingWorktree = serde_json::from_str(&plan)?;
    let PendingSource::New { base: unset @ None } = &mut pending.source else {
        return Ok(());
    };
    *unset = Some(base.to_string());
    tx.execute(
        "UPDATE pending_worktrees SET plan=?2 WHERE workspace_id=?1",
        params![id, serde_json::to_string(&pending)?],
    )?;
    match owner {
        Some(owner) if stack::builds_on(tx, owner, id)? => Ok(()),
        Some(owner) => stack::stack_on(tx, id, owner, EventCause::Blocker),
        None => Ok(()),
    }
}
