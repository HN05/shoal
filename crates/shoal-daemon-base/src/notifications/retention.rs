//! How long notifications stay, read or unread: those about a space of a
//! repository for its `cleanup.restore.retention_days` after Shoal recorded
//! them, whoever sent them, and the rest, those about no space, a chat
//! without a repository or a space whose repository Shoal no longer knows,
//! for the global retention.
use shoal_ids::repository_ids::RepositoryId;

use anyhow::Result;
use rusqlite::{Connection, Transaction, params};

use super::Notifications;
use crate::{retention::Retention, time::UnixTime};

/// The registered repository of the space a notification is about, which
/// exists or Shoal can restore.
const REPOSITORY: &str = "(SELECT id FROM repositories WHERE id=COALESCE(
        (SELECT repository_id FROM spaces WHERE id=alerts.space_id),
        (SELECT repository_id FROM removed_spaces WHERE space_id=alerts.space_id)))";

impl Notifications {
    /// Delete the notifications recorded longer ago than the retention of
    /// their space's repository, reading each repository's once.
    pub async fn expire(&self, retention: &dyn Retention) -> Result<()> {
        let repositories = self.store.run(|db| repositories(db)).await?;
        let now = UnixTime::now();
        let mut kept_from = Vec::new();
        for repository in repositories {
            let period = retention.of(repository.as_ref()).await;
            kept_from.push((repository, now - period));
        }
        self.store
            .write(move |tx| forget_before(tx, &kept_from))
            .await
    }
}

/// The repositories of the spaces notifications are about, with none for
/// those without one.
fn repositories(db: &Connection) -> Result<Vec<Option<RepositoryId>>> {
    Ok(db
        .prepare(&format!(
            "SELECT DISTINCT {REPOSITORY} FROM alerts WHERE recipient_id IS NULL"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Delete the notifications of each repository recorded before the time
/// given with it. Those of a repository that came up since are kept for the
/// next pass.
fn forget_before(
    tx: &Transaction<'_>,
    kept_from: &[(Option<RepositoryId>, UnixTime)],
) -> Result<()> {
    for (repository, time) in kept_from {
        tx.execute(
            &format!(
                "DELETE FROM alerts
                    WHERE recipient_id IS NULL AND created_at<?2 AND {REPOSITORY} IS ?1"
            ),
            params![repository, time],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
