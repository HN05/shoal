//! Grace periods of unattended removals, persisted so restarts keep them.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::time::Duration;

use crate::{daemon::workspace::Manager, time::UnixTime};

impl Manager {
    /// When the space's grace period ends, starting it now when none runs,
    /// and recording that this pass still wants to remove it.
    pub(crate) async fn claim_grace(&self, id: &SpaceId, grace: Duration) -> Result<UnixTime> {
        self.claim(
            "INSERT INTO cleanup_grace(space_id, cleanup_at, claimed) VALUES (?1, ?2, 1)
             ON CONFLICT(space_id) DO UPDATE SET claimed=1
             RETURNING cleanup_at",
            id.as_str(),
            grace,
        )
        .await
    }

    /// [`Self::claim_grace`] for removing a contribution repository.
    pub(crate) async fn claim_contribution_grace(
        &self,
        id: &str,
        grace: Duration,
    ) -> Result<UnixTime> {
        self.claim(
            "INSERT INTO contribution_grace(repository_id, cleanup_at, claimed) VALUES (?1, ?2, 1)
             ON CONFLICT(repository_id) DO UPDATE SET claimed=1
             RETURNING cleanup_at",
            id,
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
    pub(crate) async fn cleanup_at(&self, id: &SpaceId) -> Result<Option<UnixTime>> {
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
    pub(super) async fn begin_grace_pass(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute_batch(
                    "UPDATE cleanup_grace SET claimed=0; UPDATE contribution_grace SET claimed=0;",
                )?;
                Ok(())
            })
            .await
    }

    /// End the grace periods no attempt in this pass claimed: those
    /// spaces and contributions stopped qualifying, so a later attempt
    /// starts a new one.
    pub(super) async fn end_unclaimed_grace(&self) -> Result<()> {
        self.store
            .run(|db| {
                db.execute_batch(
                    "DELETE FROM cleanup_grace WHERE claimed=0;
                     DELETE FROM contribution_grace WHERE claimed=0;",
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
