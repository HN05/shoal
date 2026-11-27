//! Removal records: what `shoal restore` needs to reopen a space, kept
//! until the record expires.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::execution::recovery::{LastSession, LastSessionExt, StoppedAgent};
use crate::model::EventCause;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_store::split::{self, Column};

use crate::{
    git,
    model::{
        Checkout, Checkpoint, CheckpointCause, KeptChat, KeptWorkspace, RemovedKind, RemovedSpace,
        Space, Workspace, Worktree,
    },
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

fn kept_workspace(worktree: &Worktree, checkpoint: Checkpoint) -> RemovedKind {
    RemovedKind::Workspace(Box::new(KeptWorkspace {
        checkpoint,
        base: worktree.checkout.base().cloned(),
        head: worktree.head.clone(),
        setup_skipped: worktree.setup_skipped,
        pending_review: None,
    }))
}

/// How and where its agent ran.
fn kept_chat(space: &Space) -> RemovedKind {
    RemovedKind::Chat {
        chat: Box::new(KeptChat {
            mode: (space.mode.clone()).unwrap_or_else(crate::model::chat_mode),
            narrowing: space.narrowing,
            workspace: space.workdir.workspace().cloned(),
            root_dir: space.workdir.root_dir().map(Into::into),
        }),
    }
}

/// True when no space has the record's ID.
const GONE: &str = "space_id NOT IN (SELECT id FROM spaces)";

/// Each column but `chat`, with the field of the record it keeps.
const FIELDS: &[Column] = &[
    ("space_id", &["workspace_id"]),
    ("repository_id", &["repository_id"]),
    ("name", &["name"]),
    ("expires_at", &["expires_at"]),
    ("chat_workspace_id", &["chat", "workspace", "id"]),
    ("checkpoint_expires_at", &["checkpoint", "expires_at"]),
];

/// What a query selects to read a record.
const COLUMNS: &str =
    "space_id,repository_id,name,expires_at,chat_workspace_id,checkpoint_expires_at,record";

impl Spaces {
    /// Takes the checkpoint and records the space before any file is
    /// deleted, so an interrupted or failed removal stays restorable. Leaves
    /// out the agent `carried_on` ran as, which carries on in the chat the
    /// workspace becomes; that chat gives the record an ID of its own once
    /// it commits. Records of spaces that still exist are not listed.
    pub async fn save_removal_record(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        cause: EventCause,
        carried_on: Option<&ExecutionId>,
    ) -> Result<RemovedSpace> {
        let checkpoint = self
            .checkpoint(daemon, workspace, CheckpointCause::Removal, None)
            .await?;
        // Removal refuses link changes, so links read now are final, unlike
        // those of the snapshot taken before it started. The spaces below go
        // first, so its parent is still there.
        let Workspace { links, parent, .. } = self.workspace_by_id(&workspace.id).await?;
        // A parent on another machine stays, so a restore keeps it.
        let parent = match parent {
            Some(parent) => Some(parent),
            None => daemon.parent_elsewhere(&workspace.id).await?,
        };
        let (agents, last_sessions) = self.saved_agents(&workspace.id, carried_on)?;
        // The agents carrying on in the chat keep the session files.
        let session_files = match carried_on {
            Some(_) => Vec::new(),
            None => (self.unplaced_sessions(daemon, &Space::from(workspace.clone()))).await,
        };
        let removed = RemovedSpace {
            space_id: workspace.id.clone(),
            repository_id: workspace.repository_id.clone(),
            name: workspace.name.clone(),
            expires_at: checkpoint.expires_at,
            parent,
            links,
            agents,
            last_sessions,
            session_files,
            cause,
            removed_at: UnixTime::now(),
            kind: kept_workspace(&workspace.part, checkpoint),
        };
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// Runs before its state is deleted. Keeps how and where its agent ran,
    /// with the stopped agent and last session it saved, for the retention
    /// its files have too.
    pub async fn save_chat_removal_record(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        cause: EventCause,
    ) -> Result<RemovedSpace> {
        // As for a workspace, its links and parent are final now.
        let removed = self.present_removal_record(daemon, id, None, cause).await?;
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// Records nothing. Holds the stopped agents and last sessions, and a
    /// workspace at `checkpoint`, which the caller took, until that expires,
    /// or how and where a chat's agent ran, for the retention its files
    /// have.
    pub async fn present_removal_record(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        checkpoint: Option<Checkpoint>,
        cause: EventCause,
    ) -> Result<RemovedSpace> {
        let space = (self.find_space(id).await?).context("the space was removed meanwhile")?;
        let (agents, last_sessions) = self.saved_agents(&space.id, None)?;
        let session_files = self.unplaced_sessions(daemon, &space).await;
        let (kind, expires_at) = match (space.worktree(), checkpoint) {
            (Some(worktree), Some(checkpoint)) => {
                let expires_at = checkpoint.expires_at;
                (kept_workspace(worktree, checkpoint), expires_at)
            }
            (Some(_), None) => anyhow::bail!("a workspace's record keeps a checkpoint"),
            (None, _) => {
                let expires_at = self.removed_files_expiry(daemon, &space).await;
                (kept_chat(&space), expires_at)
            }
        };
        // A parent on another machine stays, so a move or restore keeps it.
        let parent = match space.parent.clone() {
            Some(parent) => Some(parent),
            None => daemon.parent_elsewhere(&space.id).await?,
        };
        Ok(RemovedSpace {
            space_id: space.id,
            repository_id: space.repository_id,
            name: space.name,
            parent,
            links: space.links,
            agents,
            last_sessions,
            session_files,
            cause,
            removed_at: UnixTime::now(),
            expires_at,
            kind,
        })
    }

    /// The removal deletes them with its state. Leaves out the agent
    /// `carried_on` ran as, which carries on elsewhere.
    fn saved_agents(
        &self,
        id: &SpaceId,
        carried_on: Option<&ExecutionId>,
    ) -> Result<(Vec<StoppedAgent>, Vec<LastSession>)> {
        let mut agents = crate::execution::recovery::stopped_agents(&self.paths, id)?;
        agents.retain(|agent| Some(&agent.execution) != carried_on);
        // An unreadable last session is not worth keeping the space for.
        let mut last_sessions = LastSession::all(&self.paths, id).unwrap_or_default();
        last_sessions.retain(|last| Some(&last.execution) != carried_on);
        Ok((agents, last_sessions))
    }

    /// Includes what is left of a worktree Git disowned. None may be a nested
    /// repository or a changed submodule, and a capped checkpoint's must be
    /// within the archive limit. A copy's starting files always count, since
    /// its starting checkpoint already holds them.
    pub async fn checkpoint_keeps_unsaved_files(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        cause: CheckpointCause,
    ) -> Result<bool> {
        let bytes = match self.left_files(daemon, workspace).await? {
            Some(left) => left.unsaved_bytes().await?,
            None => match crate::removal::holds_starting_files(workspace).await? {
                true => return Ok(true),
                false => git::checkpoint::unsaved_bytes(&workspace.path).await?,
            },
        };
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        if !cause.capped() {
            return Ok(true);
        }
        let settings = daemon
            .workspace_settings(workspace)
            .await?
            .into_settings()
            .cleanup
            .restore;
        Ok(bytes <= settings.archive_max_bytes())
    }

    /// The branch must have commits of its own, so the files are left over
    /// from merged work rather than work never committed, and a checkpoint
    /// must keep them.
    pub async fn unsaved_files_are_leftovers(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
    ) -> Result<bool> {
        let Checkout::Created { base, .. } = &workspace.checkout else {
            return Ok(false);
        };
        Ok(
            git::status::commits_since(&workspace.path, base.commit.as_str()).await? > 0
                && (self.checkpoint_keeps_unsaved_files(
                    daemon,
                    workspace,
                    CheckpointCause::Removal,
                ))
                .await?,
        )
    }

    pub async fn removed_workspaces(&self) -> Result<Vec<RemovedSpace>> {
        let mut removed = self.removed_spaces().await?;
        removed.retain(|removed| removed.workspace().is_some());
        Ok(removed)
    }

    /// Newest first. Expired ones are gone even before the sweep deletes
    /// them.
    pub async fn removed_spaces(&self) -> Result<Vec<RemovedSpace>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM removed_spaces WHERE expires_at > unixepoch()
                            AND {GONE} ORDER BY rowid DESC"
                    ))?
                    .query_map([], removed_space)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Another joined machine keeps them now. Their checkpoints and kept
    /// session files go as those no record owns do.
    pub async fn forget_moved_records(&self, ids: Vec<SpaceId>) -> Result<()> {
        self.store
            .write(move |tx| {
                for id in &ids {
                    tx.execute("DELETE FROM removed_spaces WHERE space_id=?1", [id])?;
                }
                Ok(())
            })
            .await
    }

    /// Keeps the record with its checkpoints for at least [`RESTORE_CLAIM`]
    /// in the same transaction, so expiry cannot delete them while the
    /// restore runs.
    pub async fn claim_removed_space(&self, selector: &SpaceSelector) -> Result<RemovedSpace> {
        let id = (selector.given_id().cloned()).with_context(|| {
            format!("no restorable space {selector}; a removed space has no number, so select it by its ID")
        })?;
        self.store
            .write(move |tx| claim(tx, &id))
            .await?
            .with_context(|| format!("no restorable space {selector}; shoal restore lists them"))
    }
}

/// Leaves out expired records.
pub fn kept_record_ids(db: &Connection) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare(&format!(
            "SELECT space_id FROM removed_spaces WHERE expires_at > unixepoch()
                AND {GONE} ORDER BY space_id"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The conversion makes a chat that keeps the ID, so the record and its
/// checkpoints get one of their own, which a restore opens the workspace
/// under beside the chat. A disposable workspace has none. Runs in the
/// transaction that commits the conversion, so one that fails leaves them
/// the workspace's.
pub fn rekey_converted(tx: &rusqlite::Transaction<'_>, id: &SpaceId) -> Result<()> {
    let recorded = tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM removed_spaces WHERE space_id=?1 AND NOT chat"),
            [id],
            removed_space,
        )
        .optional()?;
    let Some(mut removed) = recorded else {
        return Ok(());
    };
    let own = SpaceId::generate();
    super::super::workspace::checkpoints::reassign(tx, id, &own)?;
    removed.space_id = own.clone();
    removed.kept_workspace_mut()?.checkpoint.workspace_id = own.clone();
    rewrite(tx, id, &removed)?;
    // The chats the conversion removed from its worktree ran in the
    // workspace the record restores, which the ID no longer names.
    tx.execute(
        "UPDATE removed_spaces SET chat_workspace_id=?2 WHERE chat AND chat_workspace_id=?1",
        params![id, own],
    )?;
    Ok(())
}

/// Replaces one an earlier attempt left.
pub fn record(db: &Connection, removed: &RemovedSpace) -> Result<()> {
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO removed_spaces({COLUMNS},chat)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"
        ),
        rusqlite::params_from_iter(row_values(removed)?),
    )?;
    Ok(())
}

/// Writes `removed` over the record of `id`, which keeps its place among
/// the newest.
fn rewrite(db: &Connection, id: &SpaceId, removed: &RemovedSpace) -> Result<()> {
    let mut values = row_values(removed)?;
    values.push(id.to_string().into());
    db.execute(
        &format!(
            "UPDATE removed_spaces SET ({COLUMNS},chat)=(?1,?2,?3,?4,?5,?6,?7,?8) WHERE space_id=?9"
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}

/// The values of [`COLUMNS`] and `chat`, in their order.
fn row_values(removed: &RemovedSpace) -> Result<Vec<rusqlite::types::Value>> {
    let (record, mut values) = split::split(removed, FIELDS)?;
    values.extend([record.into(), removed.workspace().is_none().into()]);
    Ok(values)
}

/// Reads the columns [`COLUMNS`] names.
fn removed_space(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemovedSpace> {
    split::decode_row(row, "record", FIELDS)
}

/// As a root move that took its files there says, when it has a removal
/// record.
pub fn rehome_removed_chat(
    db: &Connection,
    id: &SpaceId,
    root_dir: &std::path::Path,
) -> Result<()> {
    let Some(mut removed) = find(db, id)? else {
        return Ok(());
    };
    if let RemovedKind::Chat { chat } = &mut removed.kind {
        chat.root_dir = Some(root_dir.to_owned());
    }
    record(db, &removed)
}

/// How long a restore that started keeps its removal record, checkpoints and
/// attached files from expiring.
pub const RESTORE_CLAIM: std::time::Duration = std::time::Duration::from_secs(3600);

/// Keeps it, its checkpoints and its attached files for at least
/// [`RESTORE_CLAIM`].
fn claim(tx: &rusqlite::Transaction<'_>, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    let Some(mut removed) = find(tx, id)? else {
        return Ok(None);
    };
    let until = UnixTime::now() + RESTORE_CLAIM;
    for (table, key, column) in [
        ("removed_spaces", "space_id", "expires_at"),
        ("removed_spaces", "space_id", "checkpoint_expires_at"),
        ("checkpoints", "workspace_id", "expires_at"),
    ] {
        tx.execute(
            &format!("UPDATE {table} SET {column}=?2 WHERE {key}=?1 AND {column} < ?2"),
            params![removed.space_id, until],
        )?;
    }
    crate::workspace_files::claim(tx, &removed.space_id, until)?;
    removed.expires_at = removed.expires_at.max(until);
    if let Ok(kept) = removed.kept_workspace_mut() {
        kept.checkpoint.expires_at = kept.checkpoint.expires_at.max(until);
    }
    Ok(Some(removed))
}

/// The record of `id`, whether or not it expired or a space has its ID.
pub fn stored(db: &Connection, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM removed_spaces WHERE space_id=?1"),
            [id],
            removed_space,
        )
        .optional()?)
}

fn find(db: &Connection, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM removed_spaces WHERE space_id=?1
                    AND expires_at > unixepoch() AND {GONE}"
            ),
            [id],
            removed_space,
        )
        .optional()?)
}

#[cfg(test)]
mod tests;
