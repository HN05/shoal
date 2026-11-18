//! What Git left of a disposable workspace's worktree it disowned: whether
//! the directory at its path is that leftover, which a removal deletes
//! without Git, and the identity of the directory a removal began deleting
//! that proves a leftover whose `.git` link went too.
use super::{Manager, identity::directory_identity};
use crate::{fsutil::off_runtime, git::worktree::Leftover, model::Workspace, removal};
use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use std::path::Path;

impl Manager {
    /// Whether Git disowned the worktree of `workspace`, whose repository is
    /// checked out at `repo`, and what is left at its path is the
    /// workspace's: a `.git` link proves it, and without one the identity
    /// of the directory a removal of it began deleting must still match.
    pub(crate) async fn disowned(&self, workspace: &Workspace, repo: &Path) -> Result<bool> {
        Ok(match removal::disowned(workspace, repo).await? {
            None => false,
            Some(Leftover::Linked) => true,
            Some(Leftover::Unlinked) => self.holds_removal_directory(workspace).await?,
        })
    }

    /// Record the identity of the directory of `workspace`, whose ownership
    /// the removal verified, before Git begins deleting it, when the
    /// workspace is disposable and so may be deleted without Git later.
    pub(in crate::daemon::workspace) async fn record_removal_directory(
        &self,
        workspace: &Workspace,
    ) -> Result<()> {
        if !workspace.disposable {
            return Ok(());
        }
        let path = workspace.path.clone();
        let identity = off_runtime::run(move || directory_identity(&path)).await?;
        let id = workspace.id.clone();
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO removal_directories(workspace_id,identity) VALUES (?1,?2)",
                    params![id, identity],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether the directory at the path of `workspace` is the one a
    /// removal of it began deleting.
    async fn holds_removal_directory(&self, workspace: &Workspace) -> Result<bool> {
        let id = workspace.id.clone();
        let recorded = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT identity FROM removal_directories WHERE workspace_id=?1",
                        [id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .await?;
        let Some(recorded) = recorded else {
            return Ok(false);
        };
        let path = workspace.path.clone();
        // A path that no longer names a directory is not the one recorded.
        off_runtime::run(move || Ok(directory_identity(&path).is_ok_and(|now| now == recorded)))
            .await
    }
}
