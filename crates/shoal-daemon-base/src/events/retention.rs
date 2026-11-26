//! The expiry of journal events, whose retention `docs/design/daemon.md`
//! states.
use shoal_ids::repository_ids::RepositoryId;

use anyhow::Result;
use rusqlite::{Connection, Transaction};

use crate::{
    retention::{Periods, Retention},
    store::Store,
    time::UnixTime,
};
use shoal_ids::space_ids::SpaceId;

/// A space that is gone, with its repository and when it went.
struct Gone {
    id: SpaceId,
    repository_id: Option<RepositoryId>,
    removed_at: UnixTime,
}

/// Forget the events of each space gone for longer than its `retention`,
/// and Shoal's own events older than the global retention.
pub async fn expire(store: &Store, retention: &dyn Retention) -> Result<()> {
    let gone = store.run(|db| gone(db)).await?;
    let expired = expired(gone, retention).await;
    let shoal_kept_from = UnixTime::now() - retention.of(None).await;
    store
        .write(move |tx| {
            forget(tx, &expired)?;
            forget_shoal_before(tx, shoal_kept_from)
        })
        .await
}

/// The IDs of the `gone` spaces whose retention after their removal has
/// passed, reading each repository's retention once.
async fn expired(gone: Vec<Gone>, retention: &dyn Retention) -> Vec<SpaceId> {
    let now = UnixTime::now();
    let mut periods = Periods::new(retention);
    let mut expired = Vec::new();
    for space in gone {
        if space.removed_at + periods.of(space.repository_id.as_ref()).await <= now {
            expired.push(space.id);
        }
    }
    expired
}

/// The spaces with events that are gone without a removal record or
/// unexpired attached files, each with when it was removed, or
/// recorded last when its removal is not in the journal. A space whose
/// repository is no longer registered takes the global retention.
fn gone(db: &Connection) -> Result<Vec<Gone>> {
    Ok(db
        .prepare(
            "SELECT space_id, (SELECT id FROM repositories WHERE id=repository), removed_at
            FROM (SELECT space_id,
                MAX(json_extract(record,'$.repository_id')) AS repository,
                COALESCE(MAX(CASE WHEN json_extract(record,'$.kind')='removed'
                    THEN created_at END), MAX(created_at)) AS removed_at
            FROM space_events
            WHERE space_id IS NOT NULL
                AND space_id NOT IN (SELECT id FROM spaces)
                AND space_id NOT IN (SELECT space_id FROM removed_spaces)
                AND space_id NOT IN (SELECT space_id FROM removed_space_files
                    WHERE expires_at > unixepoch())
            GROUP BY space_id)",
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
fn forget(tx: &Transaction<'_>, ids: &[SpaceId]) -> Result<()> {
    for id in ids {
        tx.execute(
            "UPDATE journal_pruned SET id=MAX(id, (SELECT COALESCE(MAX(id),0)
                FROM space_events WHERE space_id=?1))",
            [id],
        )?;
        tx.execute(
            "DELETE FROM space_events WHERE space_id=?1
                AND ?1 NOT IN (SELECT id FROM spaces)
                AND ?1 NOT IN (SELECT space_id FROM removed_spaces)",
            [id],
        )?;
    }
    Ok(())
}

/// Forget Shoal's own events recorded before `time`.
fn forget_shoal_before(tx: &Transaction<'_>, time: UnixTime) -> Result<()> {
    tx.execute(
        "UPDATE journal_pruned SET id=MAX(id, (SELECT COALESCE(MAX(id),0)
            FROM space_events WHERE space_id IS NULL AND created_at<?1))",
        [time],
    )?;
    tx.execute(
        "DELETE FROM space_events WHERE space_id IS NULL AND created_at<?1",
        [time],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
