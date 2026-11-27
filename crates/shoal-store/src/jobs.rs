//! How a row keeps a CI job of the runner, as the agent queue's job entries
//! and the claimed jobs do: its handle in the column `handle`, which tells
//! one attempt of the job from another, and the rest in `job` as the JSON of
//! a [`StoredJob`].
use anyhow::Result;
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use shoal_forge_types::ci::runner::{JobRun, WaitingJob};
use shoal_forge_types::numbers::{JobId, RunId};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::QueuedJob;

use crate::sql::json_column;

/// The columns that keep a job.
pub struct JobColumns {
    pub handle: String,
    pub job: String,
}

impl JobColumns {
    /// The columns that keep `job`.
    pub fn of(job: &QueuedJob) -> Result<Self> {
        let QueuedJob {
            job: waiting,
            run,
            space_ids,
            default_branch,
        } = job;
        let WaitingJob {
            handle,
            id,
            run: run_id,
            name,
            labels,
        } = waiting;
        let stored = StoredJob {
            job: StoredWaitingJob {
                id: *id,
                run: *run_id,
                name: name.clone(),
                labels: labels.clone(),
            },
            run: run.clone(),
            space_ids: space_ids.clone(),
            default_branch: *default_branch,
        };
        Ok(Self {
            handle: handle.clone(),
            job: serde_json::to_string(&stored)?,
        })
    }
}

/// The job `row` keeps, if it keeps one.
pub fn queued_job(row: &Row<'_>) -> rusqlite::Result<Option<QueuedJob>> {
    let Some(stored) = json_column::<StoredJob>(row, "job")? else {
        return Ok(None);
    };
    let StoredJob {
        job,
        run,
        space_ids,
        default_branch,
    } = stored;
    let StoredWaitingJob {
        id,
        run: run_id,
        name,
        labels,
    } = job;
    Ok(Some(QueuedJob {
        job: WaitingJob {
            handle: row.get("handle")?,
            id,
            run: run_id,
            name,
            labels,
        },
        run,
        space_ids,
        default_branch,
    }))
}

/// The job `row` keeps, which a job's entry and a claimed job must.
pub fn job(row: &Row<'_>) -> rusqlite::Result<QueuedJob> {
    queued_job(row)?.ok_or_else(|| {
        let index = row.as_ref().column_index("job").unwrap_or_default();
        rusqlite::Error::InvalidColumnType(index, "job".into(), rusqlite::types::Type::Null)
    })
}

/// What a row keeps as JSON of a [`QueuedJob`].
#[derive(Serialize, Deserialize)]
struct StoredJob {
    job: StoredWaitingJob,
    run: JobRun,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    space_ids: Vec<SpaceId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    default_branch: bool,
}

/// What a row keeps as JSON of a [`WaitingJob`].
#[derive(Serialize, Deserialize)]
struct StoredWaitingJob {
    id: JobId,
    run: RunId,
    name: String,
    labels: Vec<String>,
}

#[cfg(test)]
mod tests;
