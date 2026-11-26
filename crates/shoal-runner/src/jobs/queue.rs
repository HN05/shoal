//! The runner's entries in the agent queue.
use anyhow::Result;
use rusqlite::params;
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::model::QueuedJob;

use crate::Runner;

impl Runner {
    /// Take `repository_id`'s jobs out of the queue, as once its runner is
    /// turned off; the forge keeps them for other runners.
    pub async fn forget_job_entries(&self, repository_id: &RepositoryId) -> Result<()> {
        let (records, id) = (self.records.clone(), repository_id.to_owned());
        self.store
            .run(move |db| records.forget_job_entries(db, &id))
            .await
    }

    /// Delete `repository`'s job entry `id` if it still waits, leaving its
    /// `job` to other runners, returning whether it still waited.
    pub async fn pass_over_job(
        &self,
        id: &QueueEntryId,
        repository: &RepositoryId,
        job: &QueuedJob,
    ) -> Result<bool> {
        let records = self.records.clone();
        let (id, repository, handle) =
            (id.to_owned(), repository.to_owned(), job.job.handle.clone());
        self.store
            .write(move |tx| {
                let deleted = records.delete_waiting_entry(tx, &id)?;
                if deleted {
                    tx.execute(
                        "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle) VALUES (?1,?2)",
                        params![repository, handle],
                    )?;
                }
                Ok(deleted)
            })
            .await
    }
}
