//! The jobs the runner claimed, as it records them.
use anyhow::Result;
use rusqlite::params;
use shoal_forge_types::numbers::RunnerId;
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::model::QueuedJob;

use crate::{Runner, time::UnixTime};

#[derive(Debug, Clone)]
pub struct ClaimedJob {
    /// The ID its queue entry had.
    pub id: QueueEntryId,
    pub repository_id: RepositoryId,
    pub workspace_id: SpaceId,
    /// The execution running the forge's runner, once it started.
    pub execution_id: Option<ExecutionId>,
    pub launched_at: Option<UnixTime>,
    pub job: QueuedJob,
    /// The runner the forge registered for this job alone, if it did.
    pub runner_id: Option<RunnerId>,
}

impl Runner {
    pub async fn claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        self.store.run(|db| claims(db)).await
    }

    /// Record `execution` as the one running the claimed job `id`'s
    /// runner, and when it launched.
    pub async fn record_job_execution(
        &self,
        id: &QueueEntryId,
        execution: ExecutionId,
    ) -> Result<()> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE ci_jobs SET execution_id=?2,launched_at=coalesce(launched_at,?3)
                     WHERE id=?1",
                    params![id, execution, UnixTime::now()],
                )?;
                Ok(())
            })
            .await
    }
}

/// The jobs the runner claimed, in the order it claimed them.
pub(super) fn claims(db: &rusqlite::Connection) -> Result<Vec<ClaimedJob>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,workspace_id,execution_id,job,launched_at,runner_id
             FROM ci_jobs ORDER BY started_at",
        )?
        .query_map([], |row| {
            Ok(ClaimedJob {
                id: row.get(0)?,
                repository_id: row.get(1)?,
                workspace_id: row.get(2)?,
                execution_id: row.get(3)?,
                launched_at: row.get(5)?,
                runner_id: row.get(6)?,
                job: queued_job(row, 4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn queued_job(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<QueuedJob> {
    serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
