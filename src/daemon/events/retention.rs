//! How long a space's events stay: while the space exists, and once it is
//! gone for its repository's `cleanup.restore.retention_days` after its
//! removal, as its checkpoints and attached files do. A removal record or
//! a removed chat's unexpired files keep them longer.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::daemon::{log, workspace::Manager};

/// A space that is gone, with its repository and when it went.
struct Gone {
    id: String,
    repository_id: Option<String>,
    removed_at: i64,
}

impl Manager {
    /// Forget the events of each space gone for longer than its retention.
    pub(crate) async fn expire_events(&self) -> Result<()> {
        let gone = self.store.run(|db| gone(db)).await?;
        let expired = self.expired(gone).await;
        self.store.run(move |db| forget(db, &expired)).await
    }

    /// The IDs of the `gone` spaces whose retention after their removal
    /// has passed, reading each repository's retention once.
    async fn expired(&self, gone: Vec<Gone>) -> Vec<String> {
        let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
        let mut retention = HashMap::new();
        let mut expired = Vec::new();
        for space in gone {
            if !retention.contains_key(&space.repository_id) {
                let seconds = self.retention(space.repository_id.as_deref()).await;
                retention.insert(space.repository_id.clone(), seconds);
            }
            let kept = retention[&space.repository_id];
            if space.removed_at.saturating_add(kept) <= now {
                expired.push(space.id);
            }
        }
        expired
    }

    /// The seconds a removed space of `repository` keeps what it leaves:
    /// its settings' retention, or the global setting's for a chat without a
    /// repository or when the repository's cannot be read.
    async fn retention(&self, repository: Option<&str>) -> i64 {
        let settings = match repository {
            Some(id) => self.repository_settings(id).await,
            None => self.config().resolve(&Default::default()),
        };
        let settings = settings.or_else(|error| {
            log!("events of removed spaces kept by the global retention: {error:#}");
            self.config().resolve(&Default::default())
        });
        let seconds = settings.map_or(0, |settings| settings.cleanup.restore.retention().as_secs());
        i64::try_from(seconds).unwrap_or(i64::MAX)
    }
}

/// The spaces with events that are gone without a removal record or a
/// removed chat's unexpired files, each with when it was removed, or
/// recorded last when its removal is not in the journal.
fn gone(db: &Connection) -> Result<Vec<Gone>> {
    Ok(db
        .prepare(
            "SELECT space_id,
                MAX(COALESCE(json_extract(record,'$.repository_id'),
                    json_extract(record,'$.chat.repository_id'))),
                COALESCE(MAX(CASE WHEN json_extract(record,'$.kind') IN ('removed','chat_ended')
                    THEN created_at END), MAX(created_at))
            FROM workspace_events
            WHERE space_id NOT IN (SELECT id FROM workspaces)
                AND space_id NOT IN (SELECT id FROM chats)
                AND space_id NOT IN (SELECT workspace_id FROM removed_workspaces)
                AND space_id NOT IN (SELECT chat_id FROM ended_chats
                    WHERE expires_at > unixepoch())
            GROUP BY space_id",
        )?
        .query_map([], |row| {
            Ok(Gone {
                id: row.get(0)?,
                repository_id: row.get(1)?,
                removed_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Forget the events of the spaces `ids`, unless one came back meanwhile.
fn forget(db: &mut Connection, ids: &[String]) -> Result<()> {
    let tx = db.transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE journal_pruned SET id=MAX(id, (SELECT COALESCE(MAX(id),0)
                FROM workspace_events WHERE space_id=?1))",
            [id],
        )?;
        tx.execute(
            "DELETE FROM workspace_events WHERE space_id=?1
                AND ?1 NOT IN (SELECT id FROM workspaces)
                AND ?1 NOT IN (SELECT id FROM chats)
                AND ?1 NOT IN (SELECT workspace_id FROM removed_workspaces)",
            [id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Give the events of the removed workspace `from` to the workspace `to`
/// that restores it, in the transaction that hands it the checkpoints, so
/// its log goes on where it ended.
pub(crate) fn adopt(db: &Connection, from: &str, to: &str) -> Result<()> {
    db.execute(
        "UPDATE workspace_events SET record=json_set(record,'$.workspace_id',?2)
            WHERE space_id=?1 AND json_extract(record,'$.workspace_id')=?1",
        params![from, to],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
