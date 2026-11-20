//! The jobs the runner claimed, as it records them: each with the
//! workspace it runs in, the execution running the forge's runner once it
//! started, and the runner the forge registered for it alone.
use anyhow::Result;
use shoal_forge_types::numbers::RunnerId;
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::model::QueuedJob;

use crate::{Runner, time::UnixTime};

/// A job the runner claimed.
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    /// The ID its queue entry had.
    pub id: QueueEntryId,
    pub repository_id: RepositoryId,
    pub workspace_id: SpaceId,
    /// The execution running the forge's runner, once it started.
    pub execution_id: Option<ExecutionId>,
    /// When that execution started.
    pub launched_at: Option<UnixTime>,
    pub job: QueuedJob,
    /// The runner the forge registered for this job alone, if it did.
    pub runner_id: Option<RunnerId>,
}

impl Runner {
    /// The jobs the runner claimed.
    pub async fn claimed_jobs(&self) -> Result<Vec<ClaimedJob>> {
        self.store
            .run(|db| {
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
            })
            .await
    }
}

/// The job a row holds as JSON in its column `index`.
fn queued_job(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<QueuedJob> {
    serde_json::from_str(&row.get::<_, String>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
