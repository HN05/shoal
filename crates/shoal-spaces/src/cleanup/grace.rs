//! Grace periods of unattended removals, stored so that restarts keep them.
use crate::Spaces;
use anyhow::Result;
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;

use crate::time::UnixTime;

impl Spaces {
    /// Starts the grace period now when none runs, and records that this
    /// pass still wants to remove the space.
    pub async fn claim_grace(&self, id: &SpaceId, grace: Duration) -> Result<UnixTime> {
        self.claim(
            "INSERT INTO cleanup_grace(space_id, cleanup_at, claimed) VALUES (?1, ?2, 1)
             ON CONFLICT(space_id) DO UPDATE SET claimed=1
             RETURNING cleanup_at",
            id.as_str(),
            grace,
        )
        .await
    }

    async fn claim(&self, statement: &'static str, id: &str, grace: Duration) -> Result<UnixTime> {
        let id = id.to_owned();
        let cleanup_at = UnixTime::now() + grace;
        self.store
            .run(move |db| {
                Ok(
                    db.query_row(statement, rusqlite::params![id, cleanup_at], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
    }

    /// None while no grace period runs.
    pub async fn cleanup_at(&self, id: &SpaceId) -> Result<Option<UnixTime>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT cleanup_at FROM cleanup_grace WHERE space_id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Each removal attempt in the pass claims its grace period again.
    pub async fn begin_grace_pass(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute("UPDATE cleanup_grace SET claimed=0", [])?;
                Ok(())
            })
            .await
    }

    /// Those spaces stopped qualifying, so a later attempt starts a new
    /// grace period.
    pub async fn end_unclaimed_grace(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute("DELETE FROM cleanup_grace WHERE claimed=0", [])?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
