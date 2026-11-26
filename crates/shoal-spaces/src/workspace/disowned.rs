//! What Git left of a worktree it disowned, which a removal deletes without
//! Git.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::{
    fsutil::off_runtime,
    git::worktree::{Leftover, left::LeftFiles},
    model::Workspace,
    removal,
};
use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use shoal_daemon_base::ownership::directory_identity;
use std::path::Path;

impl Spaces {
    /// The leftover must be the workspace's: a `.git` link proves it, and
    /// without one the identity of the directory a removal began deleting
    /// must still match.
    pub async fn disowned(&self, workspace: &Workspace, repo: &Path) -> Result<bool> {
        Ok(match removal::disowned(workspace, repo).await? {
            None => false,
            Some(Leftover::Linked) => true,
            Some(Leftover::Unlinked) => self.holds_removal_directory(workspace).await?,
        })
    }

    /// Read against the branch's commit. `None` while Git knows the
    /// worktree.
    pub async fn left_files(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<Option<LeftFiles>> {
        let repo = daemon
            .repository(crate::repository::Selector::from(workspace.repository()))
            .await?;
        if !self.disowned(workspace, &repo.path).await? {
            return Ok(None);
        }
        removal::read_left(workspace, &repo.path).await.map(Some)
    }

    /// Recorded before Git begins deleting the directory, so a later removal
    /// may delete what is left without Git.
    pub async fn record_removal_directory(&self, workspace: &Workspace) -> Result<()> {
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
