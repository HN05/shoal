//! Removal records: every removal of a chat, and of an existing worktree
//! with a checkpoint, records what `shoal restore` needs to reopen the
//! space, until the record expires.
use crate::execution::recovery::{LastSession, LastSessionExt, StoppedAgent};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::daemon::workspace::Manager;
use crate::{
    daemon::events::EventCause,
    git,
    model::{
        CheckpointCause, KeptChat, KeptWorkspace, RemovedKind, RemovedSpace, Space, Workspace,
    },
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

/// SQL true when the space of a removal record is gone: no space has its ID.
const GONE: &str = "space_id NOT IN (SELECT id FROM spaces)";

impl Manager {
    /// Take the removal's checkpoint and record what `shoal restore` reopens
    /// before any file is deleted, so an interrupted or failed removal stays
    /// restorable, leaving out the agent `carried_on` ran as, which carries on
    /// in the chat the workspace becomes, which takes the record an ID of its
    /// own once it commits. Records of spaces that still exist are not
    /// listed.
    pub(in crate::daemon::workspace) async fn save_removal_record(
        &self,
        workspace: &Workspace,
        cause: EventCause,
        carried_on: Option<&ExecutionId>,
    ) -> Result<RemovedSpace> {
        let checkpoint = self
            .checkpoint(workspace, CheckpointCause::Removal, None)
            .await?;
        // Removal refuses link changes, so links read now are final, unlike
        // those of the snapshot taken before it started; the spaces below go
        // first, so its parent is still there.
        let Workspace { links, parent, .. } = self.workspace_by_id(&workspace.id).await?;
        let (agents, last_sessions) = self.saved_agents(&workspace.id, carried_on)?;
        // The agents carrying on in the chat keep the session files.
        let session_files = match carried_on {
            Some(_) => Vec::new(),
            None => (self.unplaced_sessions(&Space::from(workspace.clone()))).await,
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
            kind: RemovedKind::Workspace(Box::new(KeptWorkspace {
                checkpoint,
                base_ref: workspace.base_ref.clone(),
                base_commit: workspace.base_commit.clone(),
                head: workspace.head.clone(),
                setup_skipped: workspace.setup_skipped,
            })),
        };
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// Record what `shoal restore` reopens of the chat `id`, which `cause`
    /// removes, before its state is deleted: how and where its agent ran,
    /// with the stopped agent and last session it saved, until the
    /// retention its files have too.
    pub(in crate::daemon::workspace) async fn save_chat_removal_record(
        &self,
        id: &SpaceId,
        cause: EventCause,
    ) -> Result<RemovedSpace> {
        // As for a workspace, its links and parent are final now.
        let removed = self.chat_removal_record(id, cause).await?;
        let record = removed.clone();
        self.store.run(move |db| self::record(db, &record)).await?;
        Ok(removed)
    }

    /// What a removal of the chat `id` for `cause` would record of it now,
    /// without recording it.
    pub(crate) async fn chat_removal_record(
        &self,
        id: &SpaceId,
        cause: EventCause,
    ) -> Result<RemovedSpace> {
        let chat = (self.chat_by_id(id).await?).context("the chat was removed meanwhile")?;
        let expires_at = (self.removed_files_expiry(&Space::from(chat.clone()))).await;
        let (agents, last_sessions) = self.saved_agents(&chat.id, None)?;
        let session_files = (self.unplaced_sessions(&Space::from(chat.clone()))).await;
        let removed = RemovedSpace {
            space_id: chat.id,
            repository_id: chat.repository_id,
            name: chat.name,
            parent: chat.parent,
            links: chat.links,
            agents,
            last_sessions,
            session_files,
            cause,
            removed_at: UnixTime::now(),
            expires_at,
            kind: RemovedKind::Chat {
                chat: Box::new(KeptChat {
                    mode: (chat.mode).unwrap_or_else(crate::model::chat_mode),
                    narrowing: chat.narrowing,
                    workspace: chat.workdir.workspace().cloned(),
                    root_dir: chat.workdir.root_dir().map(Into::into),
                }),
            },
        };
        Ok(removed)
    }

    /// The stopped agents and last sessions the space `id` saved, which its
    /// removal deletes with its state, leaving out the agent `carried_on`
    /// ran as, which carries on elsewhere.
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

    /// Whether a checkpoint taken for `cause` would keep all of the
    /// workspace's unsaved files: none is a nested repository or a changed
    /// submodule, and a capped checkpoint's are within the archive limit; or
    /// they are a copy's starting files, which its starting checkpoint
    /// already holds.
    pub(crate) async fn checkpoint_keeps_unsaved_files(
        &self,
        workspace: &Workspace,
        cause: CheckpointCause,
    ) -> Result<bool> {
        if crate::removal::holds_starting_files(workspace).await? {
            return Ok(true);
        }
        let Some(bytes) = git::checkpoint::unsaved_bytes(&workspace.path).await? else {
            return Ok(false);
        };
        if !cause.capped() {
            return Ok(true);
        }
        let settings = self
            .workspace_settings(workspace)
            .await?
            .into_settings()
            .cleanup
            .restore;
        Ok(bytes <= settings.archive_max_bytes())
    }

    /// Whether merged cleanup may archive the workspace's unsaved files: the
    /// branch has commits of its own, so they are left over from merged work
    /// rather than work never committed, and a checkpoint keeps them.
    pub(in crate::daemon::workspace) async fn unsaved_files_are_leftovers(
        &self,
        workspace: &Workspace,
    ) -> Result<bool> {
        let Some(base) = &workspace.base_commit else {
            return Ok(false);
        };
        Ok(
            git::status::commits_since(&workspace.path, base.as_str()).await? > 0
                && (self.checkpoint_keeps_unsaved_files(workspace, CheckpointCause::Removal))
                    .await?,
        )
    }

    /// The workspaces among [`Self::removed_spaces`].
    pub async fn removed_workspaces(&self) -> Result<Vec<RemovedSpace>> {
        let mut removed = self.removed_spaces().await?;
        removed.retain(|removed| removed.workspace().is_some());
        Ok(removed)
    }

    /// Restorable spaces, newest first; expired ones are gone even before
    /// the sweep deletes them.
    pub async fn removed_spaces(&self) -> Result<Vec<RemovedSpace>> {
        self.store
            .run(|db| {
                db.prepare(&format!(
                    "SELECT record FROM removed_spaces WHERE expires_at > unixepoch()
                            AND {GONE} ORDER BY rowid DESC"
                ))?
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|record| Ok(serde_json::from_str(&record?)?))
                .collect()
            })
            .await
    }

    /// The removal record to reopen, kept with its checkpoints for at least
    /// [`RESTORE_CLAIM`] in the same transaction, so expiry cannot delete
    /// them while the restore runs.
    pub(in crate::daemon::workspace) async fn claim_removed_space(
        &self,
        selector: &SpaceSelector,
    ) -> Result<RemovedSpace> {
        let id = (selector.given_id().cloned()).with_context(|| {
            format!("no restorable space {selector}; a removed space has no number, so select it by its ID")
        })?;
        self.store
            .write(move |tx| claim(tx, &id))
            .await?
            .with_context(|| format!("no restorable space {selector}; shoal restore lists them"))
    }
}

/// Give the removal record of the workspace `id`, which a conversion makes a
/// chat keeping that ID, and its checkpoints an ID of their own, which its
/// restore opens it under beside the chat; a disposable workspace has none.
/// It runs in the transaction that commits the conversion, so a conversion
/// that fails leaves the record and checkpoints the workspace's.
pub(in crate::daemon::workspace) fn rekey_converted(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
) -> Result<()> {
    let recorded = crate::daemon::store::exists(
        tx,
        "SELECT 1 FROM removed_spaces WHERE space_id=?1 AND NOT chat",
        [id],
    )?;
    if !recorded {
        return Ok(());
    }
    let own = SpaceId::generate();
    super::super::checkpoints::reassign(tx, id, &own)?;
    tx.execute(
        "UPDATE removed_spaces SET space_id=?2, record=json_set(record,
            '$.workspace_id', ?2, '$.checkpoint.workspace_id', ?2)
         WHERE space_id=?1 AND NOT chat",
        params![id, own],
    )?;
    // The chats the conversion removed from its worktree ran in the
    // workspace the record restores, which the ID no longer names.
    tx.execute(
        "UPDATE removed_spaces SET record=json_set(record, '$.chat.workspace.id', ?2)
         WHERE chat AND json_extract(record, '$.chat.workspace.id')=?1",
        params![id, own],
    )?;
    Ok(())
}

/// Persist a removal record, replacing one an earlier attempt left.
pub(super) fn record(db: &Connection, removed: &RemovedSpace) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
            VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            removed.space_id,
            removed.repository_id,
            removed.workspace().is_none(),
            removed.name,
            removed.expires_at,
            serde_json::to_string(removed)?,
        ],
    )?;
    Ok(())
}

/// Record that the removed chat `id` runs in `root_dir` once restored, as a
/// root move that took its files there says, when it has a removal record.
pub(in crate::daemon::workspace) fn rehome_removed_chat(
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
pub(super) const RESTORE_CLAIM: std::time::Duration = std::time::Duration::from_secs(3600);

/// Find a removal record and keep it, its checkpoints and its attached files
/// for at least [`RESTORE_CLAIM`].
fn claim(tx: &rusqlite::Transaction<'_>, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    let Some(mut removed) = find(tx, id)? else {
        return Ok(None);
    };
    let until = UnixTime::now() + RESTORE_CLAIM;
    for (table, column) in [
        ("removed_spaces", "space_id"),
        ("checkpoints", "workspace_id"),
    ] {
        tx.execute(
            &format!(
                "UPDATE {table} SET expires_at=?2, record=json_set(record, '$.expires_at', ?2)
                 WHERE {column}=?1 AND expires_at < ?2"
            ),
            params![removed.space_id, until],
        )?;
    }
    tx.execute(
        "UPDATE removed_spaces
         SET record=json_set(record, '$.checkpoint.expires_at', ?2)
         WHERE space_id=?1 AND json_extract(record, '$.checkpoint.expires_at') < ?2",
        params![removed.space_id, until],
    )?;
    crate::daemon::workspace_files::claim(tx, &removed.space_id, until)?;
    removed.expires_at = removed.expires_at.max(until);
    if let Ok(kept) = removed.kept_workspace_mut() {
        kept.checkpoint.expires_at = kept.checkpoint.expires_at.max(until);
    }
    Ok(Some(removed))
}

/// The unexpired removal record of the removed space `id`.
fn find(db: &Connection, id: &SpaceId) -> Result<Option<RemovedSpace>> {
    let record: Option<String> = db
        .query_row(
            &format!(
                "SELECT record FROM removed_spaces WHERE space_id=?1
                    AND expires_at > unixepoch() AND {GONE}"
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(record
        .map(|record| serde_json::from_str(&record))
        .transpose()?)
}

#[cfg(test)]
mod tests;
