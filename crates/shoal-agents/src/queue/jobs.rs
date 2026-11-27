//! The queue's CI job entries, which the runner queues, admits and forgets.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::{space_names::SpaceName, time::UnixTime};

use crate::{
    model::QueuedJob,
    state::{QueueKind, QueueState},
};

/// A job's entry in the queue.
#[derive(Debug, Clone)]
pub struct JobEntry {
    pub id: QueueEntryId,
    pub repository: RepositoryId,
    pub name: SpaceName,
    pub job: QueuedJob,
}

/// Queue `job` of `repository` behind the work already waiting.
pub fn queue(db: &Connection, repository: &RepositoryId, job: &QueuedJob) -> Result<()> {
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
            UnixTime::now(),
            serde_json::to_string(job)?,
        ],
    )?;
    Ok(())
}

/// The job entries in `state`, of every repository or of `repository`
/// alone.
pub fn in_state(
    db: &Connection,
    state: QueueState,
    repository: Option<&RepositoryId>,
) -> Result<Vec<JobEntry>> {
    let rows = db
        .prepare(
            "SELECT id,repository_id,name,job FROM agent_queue
             WHERE kind=?1 AND state=?2 AND (?3 IS NULL OR repository_id=?3)
             ORDER BY seq",
        )?
        .query_map(params![QueueKind::Job, state, repository], |row| {
            let Some(job) = shoal_store::json_column(row, "job")? else {
                return Ok(None);
            };
            Ok(Some(JobEntry {
                id: row.get("id")?,
                repository: row.get("repository_id")?,
                name: row.get("name")?,
                job,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().flatten().collect())
}

/// Forget the job entry `id`, waiting or admitted.
pub fn remove(db: &Connection, id: &QueueEntryId) -> Result<()> {
    db.execute(
        "DELETE FROM agent_queue WHERE id=?1 AND kind=?2",
        params![id, QueueKind::Job],
    )?;
    Ok(())
}

/// Forget every job entry of `repository`.
pub fn forget_all(db: &Connection, repository: &RepositoryId) -> Result<()> {
    db.execute(
        "DELETE FROM agent_queue WHERE repository_id=?1 AND kind=?2",
        params![repository, QueueKind::Job],
    )?;
    Ok(())
}

/// Forget `repository`'s waiting job entries whose handles are not among
/// `listed`, returning the handles of those it keeps: listed, or admitted.
pub fn forget_unlisted(
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
        .collect::<rusqlite::Result<Vec<(QueueEntryId, String, QueueState)>>>()?;
    let mut kept = HashSet::new();
    for (id, handle, state) in entries {
        if state == QueueState::Queued && !listed.contains(&handle) {
            remove(db, &id)?;
        } else {
            kept.insert(handle);
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests;
