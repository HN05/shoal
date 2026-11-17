//! Pending worktrees: how a recorded workspace that has no worktree yet
//! gets one, which its preparation creates.

use super::{Configured, WorkspaceSource};
use crate::{daemon::workspace::Manager, model::Workspace, state::WorkspaceState};
use anyhow::{Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use shoal_protocol::space_names::SpaceSelector;

/// How the worktree of a recorded workspace that has none yet is created,
/// as `pending_worktrees` keeps it until it is.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct PendingWorktree {
    source: PendingSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_profile: Option<String>,
}

/// What a pending worktree checks out, as [`WorkspaceSource`] names it
/// without what only a restore writes into it.
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

    /// What creating the worktree checks out.
    fn source(self) -> WorkspaceSource {
        match self.source {
            PendingSource::New { base } => WorkspaceSource::New { base, parent: None },
            PendingSource::Existing { branch, base } => WorkspaceSource::Existing {
                branch,
                base,
                files: None,
            },
        }
    }
}

impl Manager {
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
        let pending = self.claim_pending_worktree(&workspace).await?;
        let git_profile = pending.git_profile.clone();
        let result = match workspace.git_dir {
            // An earlier try created it, then failed to configure it.
            Some(_) => {
                self.reconfigure_worktree(&workspace, git_profile.as_deref())
                    .await
            }
            None => {
                let source = pending.source();
                (self.materialize_worktree(&repo, &workspace, source, git_profile.as_deref())).await
            }
        };
        self.settle_creation(&repo, &workspace, result, git).await
    }

    /// Configure the worktree an earlier creation of `workspace` left once
    /// it verifies as the one Git created for it.
    async fn reconfigure_worktree(
        &self,
        workspace: &Workspace,
        git_profile: Option<&str>,
    ) -> Result<Configured> {
        self.verify_worktree(workspace).await?;
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
