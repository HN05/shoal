//! The agent queue's rows, and the executions, that the runner reads or
//! changes in its own transactions.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::{
    execution_ids::ExecutionId, queue_entry_ids::QueueEntryId, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use shoal_protocol::space_names::SpaceName;
use shoal_runner::daemon::{RecordedExecutions, WaitingEntry};

use crate::daemon::agent_service::AgentRecords;
use crate::{
    model::QueuedJob,
    state::{QueueKind, QueueState},
};

/// The agent queue's rows the runner changes in its own transactions.
pub(crate) struct RunnerRecords;

impl shoal_runner::daemon::Records for RunnerRecords {
    fn forget_job_entries(&self, db: &Connection, repository: &RepositoryId) -> Result<()> {
        db.execute(
            "DELETE FROM agent_queue WHERE repository_id=?1 AND kind=?2",
            params![repository, QueueKind::Job],
        )?;
        Ok(())
    }

    fn waiting_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        let rows = db
            .prepare(
                "SELECT id,repository_id,job FROM agent_queue
                 WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
            )?
            .query_map(params![QueueKind::Job, QueueState::Queued], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
        let mut jobs = Vec::new();
        for (id, repository, job) in rows {
            jobs.push((id, repository, serde_json::from_str(&job)?));
        }
        Ok(jobs)
    }

    fn waiting_job_entries(
        &self,
        db: &Connection,
        repository: &RepositoryId,
    ) -> Result<Vec<WaitingEntry>> {
        let entries = db
            .prepare(
                "SELECT id,name,job FROM agent_queue
                 WHERE repository_id=?1 AND kind=?2 AND state=?3",
            )?
            .query_map(
                params![repository, QueueKind::Job, QueueState::Queued],
                |row| {
                    let job = crate::daemon::store::json_column(row, "job")?;
                    Ok((row.get("id")?, row.get("name")?, job))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((entries.into_iter())
            .filter_map(|(id, name, job)| {
                Some(WaitingEntry {
                    id,
                    name,
                    job: job?,
                })
            })
            .collect())
    }

    fn admitted_jobs(
        &self,
        db: &Connection,
    ) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
        let rows = db
            .prepare(
                "SELECT id,repository_id,job FROM agent_queue
                 WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
            )?
            .query_map(params![QueueKind::Job, QueueState::Starting], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
        let mut jobs = Vec::new();
        for (id, repository, job) in rows {
            jobs.push((id, repository, serde_json::from_str(&job)?));
        }
        Ok(jobs)
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
        db.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
        Ok(())
    }

    fn return_job_entry(&self, db: &Connection, id: &QueueEntryId) -> Result<()> {
        db.execute(
            "UPDATE agent_queue SET state=?2 WHERE id=?1 AND state=?3",
            params![id, QueueState::Queued, QueueState::Starting],
        )?;
        Ok(())
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
        let entries = db
            .prepare(
                "SELECT id,json_extract(job,'$.job.handle'),state FROM agent_queue
                 WHERE repository_id=?1 AND kind=?2",
            )?
            .query_map(params![repository, QueueKind::Job], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<(String, String, QueueState)>>>()?;
        let mut kept = HashSet::new();
        for (id, handle, state) in entries {
            if state == QueueState::Queued && !listed.contains(&handle) {
                db.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
            } else {
                kept.insert(handle);
            }
        }
        Ok(kept)
    }

    fn queue_job(&self, db: &Connection, repository: &RepositoryId, job: &QueuedJob) -> Result<()> {
        let name = SpaceName::derive(&format!("ci-{}-{}", job.job.name, job.job.id), "workspace");
        db.execute(
            "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job)
             VALUES (?1,?2,?3,?4,'',?5,?6,?7)",
            params![
                QueueEntryId::generate(),
                QueueKind::Job,
                repository,
                name,
                QueueState::Queued,
                crate::time::UnixTime::now(),
                serde_json::to_string(job)?,
            ],
        )?;
        Ok(())
    }
}
