//! Files attached to workspaces, such as those sent with a message to their
//! agents. Each repository keeps Shoal's copies beside its workspaces, in a
//! directory its agents are given as a working directory, and the store
//! records each copy with how it came, so `shoal files` lists them.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;

use crate::{
    daemon::workspace::Manager,
    model::Workspace,
    state::{WorkspaceState, states},
};

pub(crate) mod layout;
mod retention;

use layout::{Kept, Layout};

#[cfg(test)]
mod tests;

states!(FileSource {
    /// Sent with a message to the workspace's agents.
    Message => "message",
    /// Added with `shoal files add`.
    Added => "added",
    /// Handed back with `shoal done`; kept after the workspace's removal for
    /// as long as its removal record.
    Result => "result",
});

/// A file attached to a workspace: Shoal's copy and how it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: i64,
    /// The workspace's name when the file was attached.
    pub workspace: String,
    pub source: FileSource,
    /// The attached file's name.
    pub name: String,
    /// Shoal's copy.
    pub path: PathBuf,
    pub bytes: u64,
    /// Unix seconds.
    pub created_at: i64,
    /// When a removed workspace's file is deleted, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

const COLUMNS: &str = "id,workspace,source,name,path,bytes,created_at,
    (SELECT expires_at FROM removed_workspaces r WHERE r.workspace_id=workspace_files.workspace_id
        AND workspace_files.workspace_id NOT IN (SELECT id FROM workspaces))";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    Ok(AttachedFile {
        id: row.get(0)?,
        workspace: row.get(1)?,
        source: row.get(2)?,
        name: row.get(3)?,
        path: PathBuf::from(row.get::<_, String>(4)?),
        bytes: row.get::<_, i64>(5)?.try_into().unwrap_or_default(),
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

/// The workspace whose attached files a request names: one that exists, or
/// a removed one whose results are kept.
#[derive(Debug, Clone)]
pub(crate) struct Owner {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) repository_id: String,
    /// The workspace's parent when it attached the files.
    pub(crate) parent_id: Option<String>,
}

impl From<Workspace> for Owner {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            repository_id: workspace.repository_id,
            parent_id: workspace.parent_workspace.map(|parent| parent.id),
        }
    }
}

/// Record the copies `kept` as attached to the workspace `workspace_id` from
/// `source`, in the caller's transaction. A copy already recorded, as when
/// the same files are sent again, keeps its record.
pub(crate) fn record(
    tx: &Transaction<'_>,
    workspace_id: &str,
    source: FileSource,
    paths: &[PathBuf],
    bytes: &[u64],
) -> Result<()> {
    for (path, bytes) in paths.iter().zip(bytes) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        tx.execute(
            "INSERT OR IGNORE INTO workspace_files
                (workspace_id,workspace,parent_id,repository_id,source,name,path,bytes,created_at)
                SELECT id,name,parent_workspace_id,repository_id,?2,?3,?4,?5,unixepoch()
                FROM workspaces WHERE id=?1",
            params![
                workspace_id,
                source,
                name,
                path.to_string_lossy(),
                i64::try_from(*bytes)?
            ],
        )?;
    }
    Ok(())
}

/// The text copies of added files are kept under, which tells them from a
/// message that is only its files.
const ADDED: &str = "shoal files add";

/// The records of the copies `paths`, in their order.
fn recorded(tx: &Transaction<'_>, paths: &[PathBuf]) -> Result<Vec<AttachedFile>> {
    let mut statement = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM workspace_files WHERE path=?1"
    ))?;
    paths
        .iter()
        .map(|path| Ok(statement.query_row([path.to_string_lossy()], row)?))
        .collect()
}

/// Point the recorded copies of repository `repository_id` from `source` to
/// `destination`, in the transaction that moves the repository's directory.
pub(crate) fn rebase(
    tx: &Transaction<'_>,
    repository_id: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let recorded: Vec<(i64, String)> = tx
        .prepare("SELECT id,path FROM workspace_files WHERE repository_id=?1")?
        .query_map([repository_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, path) in recorded {
        if let Ok(inside) = Path::new(&path).strip_prefix(source) {
            tx.execute(
                "UPDATE workspace_files SET path=?2 WHERE id=?1",
                params![id, destination.join(inside).to_string_lossy()],
            )?;
        }
    }
    Ok(())
}

/// Copies of `files`, attached to `message` for the workspace
/// `workspace_id`, made off the async runtime. Callers hold the gate of the
/// repository's attached files.
pub(crate) async fn keep(
    layout: Layout,
    workspace_id: &str,
    message: &str,
    files: Vec<PathBuf>,
) -> Result<Kept> {
    let (id, message) = (workspace_id.to_owned(), message.to_owned());
    tokio::task::spawn_blocking(move || layout.keep(&id, &message, &files)).await?
}

/// Delete the copies of files that were refused, such as ones for a
/// workspace removed while they were made. Failure only logs: the workspace's
/// removal deletes whatever is left.
pub(crate) async fn discard(kept: Kept) {
    let discarded = tokio::task::spawn_blocking(move || kept.discard()).await;
    if let Err(error) = discarded
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        crate::daemon::log!("refused attached files not deleted: {error:#}");
    }
}

impl Manager {
    /// The files attached to the workspace `selector` names, or that a
    /// removed workspace of that name handed back, oldest first.
    pub async fn workspace_files(&self, selector: &str) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM workspace_files WHERE workspace_id=?1 ORDER BY id"
                    ))?
                    .query_map([&owner.id], row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// The workspace `selector` names, or else the latest removed one of
    /// that name or ID whose results are kept.
    pub(crate) async fn file_owner(&self, selector: &str) -> Result<Owner> {
        let error = match self.workspace(selector).await {
            Ok(workspace) => return Ok(workspace.into()),
            Err(error) => error,
        };
        let selector = selector.to_owned();
        let removed = self
            .store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT workspace_id,workspace,repository_id,parent_id FROM workspace_files
                            WHERE (workspace_id=?1 OR workspace=?1)
                                AND workspace_id NOT IN (SELECT id FROM workspaces)
                            ORDER BY id DESC LIMIT 1",
                        [selector],
                        |row| {
                            Ok(Owner {
                                id: row.get(0)?,
                                name: row.get(1)?,
                                repository_id: row.get(2)?,
                                parent_id: row.get(3)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await?;
        removed.ok_or(error)
    }

    /// Copies of the files `results` that `workspace` hands back with its
    /// completion, recorded as its results, which outlive its removal.
    pub(crate) async fn keep_results(
        &self,
        workspace: &Workspace,
        results: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>> {
        if results.is_empty() {
            return Ok(results);
        }
        let (layout, _files) = self.lock_files(workspace).await?;
        let id = workspace.id.clone();
        let kept =
            tokio::task::spawn_blocking(move || layout.keep_results(&id, &results)).await??;
        let records = self
            .record_or_discard(&workspace.id, FileSource::Result, kept)
            .await?;
        Ok(records.into_iter().map(|file| file.path).collect())
    }

    /// Delete the files attached to the workspace `selector` names, only the
    /// copies of files called `name` when given, and return their records.
    pub async fn remove_workspace_files(
        &self,
        selector: &str,
        name: Option<String>,
    ) -> Result<Vec<AttachedFile>> {
        let owner = self.file_owner(selector).await?;
        let _files = self.gates.lock_message_files(&owner.repository_id).await;
        let (id, wanted) = (owner.id.clone(), name.clone());
        let files: Vec<AttachedFile> = self
            .store
            .run(move |db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM workspace_files
                            WHERE workspace_id=?1 AND (?2 IS NULL OR name=?2) ORDER BY id"
                    ))?
                    .query_map(params![id, wanted], row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        if let Some(name) = &name {
            ensure!(
                !files.is_empty(),
                "workspace {} has no attached file named {name}",
                owner.name
            );
        }
        self.delete_files(files).await
    }

    /// Delete the attached files of every workspace without a running agent
    /// or command, and return their records.
    pub async fn remove_idle_files(&self) -> Result<Vec<AttachedFile>> {
        let mut deleted = Vec::new();
        for repo in self.repositories().await? {
            let _files = self.gates.lock_message_files(&repo.id).await;
            let id = repo.id.clone();
            let files = self
                .store
                .run(move |db| {
                    Ok(db
                        .prepare(&format!(
                            "SELECT {COLUMNS} FROM workspace_files WHERE repository_id=?1
                                AND workspace_id NOT IN
                                    (SELECT workspace_id FROM executions WHERE state=?2)
                                ORDER BY id"
                        ))?
                        .query_map(params![id, crate::state::ExecutionState::Running], row)?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                })
                .await?;
            deleted.extend(self.delete_files(files).await?);
        }
        Ok(deleted)
    }

    /// Delete the copies `files` and then their records. Callers hold the
    /// gate of their repository's attached files.
    async fn delete_files(&self, files: Vec<AttachedFile>) -> Result<Vec<AttachedFile>> {
        let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
        tokio::task::spawn_blocking(move || {
            paths.iter().try_for_each(|path| layout::delete_copy(path))
        })
        .await??;
        let ids: Vec<i64> = files.iter().map(|file| file.id).collect();
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in ids {
                    tx.execute("DELETE FROM workspace_files WHERE id=?1", [id])?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(files)
    }

    /// Attach copies of `files` to the workspace `selector` names, without a
    /// message, and return their records. Adding the same files again keeps
    /// the copies they have.
    pub async fn add_workspace_files(
        &self,
        selector: &str,
        files: Vec<PathBuf>,
    ) -> Result<Vec<AttachedFile>> {
        ensure!(!files.is_empty(), "give the files to add");
        let workspace = self.workspace(selector).await?;
        let (layout, _files) = self.lock_files(&workspace).await?;
        let kept = keep(layout, &workspace.id, ADDED, files).await?;
        self.record_or_discard(&workspace.id, FileSource::Added, kept)
            .await
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source` and return their records, or delete the copies made for
    /// them when that fails.
    async fn record_or_discard(
        &self,
        workspace_id: &str,
        source: FileSource,
        kept: Kept,
    ) -> Result<Vec<AttachedFile>> {
        let recorded = self.record_kept(workspace_id, source, &kept).await;
        if recorded.is_err() {
            discard(kept).await;
        }
        recorded
    }

    /// Record the copies `kept` as attached to the workspace `workspace_id`
    /// from `source`, and return their records.
    async fn record_kept(
        &self,
        workspace_id: &str,
        source: FileSource,
        kept: &Kept,
    ) -> Result<Vec<AttachedFile>> {
        let id = workspace_id.to_owned();
        let (paths, bytes) = (kept.paths.clone(), kept.bytes.clone());
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                record(&tx, &id, source, &paths, &bytes)?;
                let records = recorded(&tx, &paths)?;
                tx.commit()?;
                Ok(records)
            })
            .await
    }

    /// Hold the gate of the files attached to `workspace`, with
    /// where its repository keeps them, once the workspace is still there and
    /// not being removed. A registration older than the repository's
    /// directory reserves it first, under the Git gate, which is never taken
    /// while holding this one.
    pub(crate) async fn lock_files(
        &self,
        workspace: &Workspace,
    ) -> Result<(Layout, OwnedMutexGuard<()>)> {
        let repo = self.repository(&workspace.repository_id).await?;
        if repo.workspaces_dir.is_none() {
            let (repo, _git) = self.lock_repository(&repo.id).await?;
            self.workspaces_dir(&repo).await?;
        }
        let guard = self.gates.lock_message_files(&repo.id).await;
        // A move or removal may have finished while this waited.
        let current = self.workspace(&workspace.id).await?;
        ensure!(
            current.state != WorkspaceState::Removing,
            "the workspace is being removed"
        );
        let repo = self.repository(&repo.id).await?;
        let layout = Layout::of(&repo).context("the repository has no directory")?;
        Ok((layout, guard))
    }
}
