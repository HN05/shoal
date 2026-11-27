//! The agent queue's rows, and the executions, that the runner reads or
//! changes in its own transactions.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_runner::daemon::{RecordedExecutions, WaitingEntry};

use crate::daemon::agent_queue::jobs::{self, JobEntry};
use crate::daemon::agent_service::AgentRecords;
use crate::{model::QueuedJob, state::QueueState};

/// The agent queue's rows the runner changes in its own transactions.
pub(crate) struct RunnerRecords;

impl shoal_runner::daemon::Records for RunnerRecords {
    fn forget_job_entries(&self, db: &Connection, repository: &RepositoryId) -> Result<()> {
        jobs::forget_all(db, repository)
    }

    fn waiting_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        jobs_in(db, QueueState::Queued)
    }

    fn waiting_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
    ) -> Result<Vec<WaitingEntry>> {
        let entries = jobs::in_state(db, QueueState::Queued, Some(repository))?;
        Ok((entries.into_iter())
            .map(|entry| WaitingEntry {
                id: entry.id,
                name: entry.name,
                job: entry.job,
            })
            .collect())
    }

    fn admitted_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        jobs_in(db, QueueState::Starting)
    }

    fn recorded_executions(&self, db: &Connection) -> Result<RecordedExecutions> {
        let mut recorded = RecordedExecutions::default();
        let rows = db
            .prepare("SELECT id,space_id FROM executions")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(ExecutionId, SpaceId)>>>()?;
        for (id, space) in rows {
            recorded.ids.insert(id);
            recorded.spaces.insert(space);
        }
        Ok(recorded)
    }

    fn remove_queue_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()> {
        jobs::remove(db, id)
    }

    fn return_job_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()> {
        crate::daemon::agent_queue::store::return_start(db, id)
    }

    fn delete_waiting_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<bool> {
        crate::daemon::agent_queue::actions::delete_waiting(db, &AgentRecords, id)
    }

    fn forget_unlisted_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
        listed: &HashSet<String>,
    ) -> Result<HashSet<String>> {
        jobs::forget_unlisted(db, repository, listed)
    }

    fn queue_job(&self, db: &Connection, repository: &RepositoryId, job: &QueuedJob) -> Result<()> {
        jobs::queue(db, repository, job)
    }
}

/// Every repository's job entries in `state`, as the runner reads them.
fn jobs_in(
    db: &Connection,
    state: QueueState,
) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
    let entries = jobs::in_state(db, state, None)?;
    Ok((entries.into_iter())
        .map(
            |JobEntry {
                 id,
                 repository,
                 job,
                 ..
             }| (id, repository, job),
        )
        .collect())
}
