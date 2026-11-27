//! The queue's CI job entries, which the runner queues, admits and forgets.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};
use shoal_protocol::{space_names::SpaceName, time::UnixTime};

use shoal_store::jobs::JobColumns;

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
    let columns = JobColumns::of(job)?;
    db.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job,handle)
         VALUES (?1,?2,?3,?4,'',?5,?6,?7,?8)",
        params![
            QueueEntryId::generate(),
            QueueKind::Job,
            repository,
            name,
            QueueState::Queued,
            UnixTime::now(),
            columns.job,
            columns.handle,
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
    Ok(db
        .prepare(
            "SELECT id,repository_id,name,job,handle FROM agent_queue
             WHERE kind=?1 AND state=?2 AND (?3 IS NULL OR repository_id=?3)
             ORDER BY seq",
        )?
        .query_map(params![QueueKind::Job, state, repository], |row| {
            Ok(JobEntry {
                id: row.get("id")?,
                repository: row.get("repository_id")?,
                name: row.get("name")?,
                job: shoal_store::jobs::job(row)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
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
    let mut kept = HashSet::new();
    for entry in handled_entries(db, repository)? {
        if entry.state == QueueState::Queued && !listed.contains(&entry.handle) {
            remove(db, &entry.id)?;
        } else {
            kept.insert(entry.handle);
        }
    }
    Ok(kept)
}

/// A job entry with the forge's handle of its job.
struct HandledEntry {
    id: QueueEntryId,
    handle: String,
    state: QueueState,
}

/// Each job entry of `repository`, waiting or admitted, with its job's
/// handle.
fn handled_entries(db: &Connection, repository: &RepositoryId) -> Result<Vec<HandledEntry>> {
    Ok(db
        .prepare("SELECT id,handle,state FROM agent_queue WHERE repository_id=?1 AND kind=?2")?
        .query_map(params![repository, QueueKind::Job], |row| {
            Ok(HandledEntry {
                id: row.get(0)?,
                handle: row.get(1)?,
                state: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
