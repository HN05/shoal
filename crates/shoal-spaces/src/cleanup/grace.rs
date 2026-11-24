//! Grace periods of unattended removals, persisted so restarts keep them.
use crate::Spaces;
use anyhow::Result;
use rusqlite::OptionalExtension;
use shoal_ids::space_ids::SpaceId;
use std::time::Duration;

use crate::time::UnixTime;

impl Spaces {
    /// When the space's grace period ends, starting it now when none runs,
    /// and recording that this pass still wants to remove it.
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

    /// When automatic cleanup removes the space, while its grace period runs.
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

    /// Start a sweep pass: each removal attempt in it claims its grace period again.
    pub async fn begin_grace_pass(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute("UPDATE cleanup_grace SET claimed=0", [])?;
                Ok(())
            })
            .await
    }

    /// End the grace periods no attempt in this pass claimed: those
    /// spaces stopped qualifying, so a later attempt starts a new one.
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
