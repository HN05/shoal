//! The waiting entries as ranking sees them: the queue's waiting rows in
//! the order they arrived, then the one-shot runs waiting for a slot.
use anyhow::{Context, Result};
use rusqlite::Connection;

use super::super::{
    rank::{Entry, Work},
    state::WaitingOneShot,
};
use crate::client::{queue::QueueEntryId, space_ids::SpaceId, time::UnixTime};
use crate::{
    forge::ci::runner::Trigger,
    model::{QueuedJob, Tier},
    state::{QueueKind, QueueState},
};

/// The waiting entries, the queue's then `oneshots`.
pub(super) fn waiting(db: &Connection, oneshots: &[WaitingOneShot]) -> Result<Vec<Entry>> {
    let rows = db
        .prepare(
            "SELECT id,kind,name,created_at,COALESCE(workspace_id,opens_workspace_id),job
             FROM agent_queue WHERE state=?1 ORDER BY seq",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok((
                row.get::<_, QueueEntryId>(0)?,
                row.get::<_, QueueKind>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, UnixTime>(3)?,
                row.get::<_, Option<SpaceId>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut entries = Vec::new();
    for (id, kind, name, queued_at, space, job) in rows {
        entries.push(Entry {
            id,
            name,
            work: work(kind, space, job.as_deref())?,
            queued_at,
            arrival: entries.len(),
        });
    }
    for oneshot in oneshots {
        entries.push(Entry {
            id: oneshot.id.clone(),
            name: "review".into(),
            work: Work::Review {
                space: oneshot.space.clone(),
            },
            queued_at: oneshot.requested_at,
            arrival: entries.len(),
        });
    }
    Ok(entries)
}

/// What an entry of `kind` in `space` starts; `job` is a job's entry's job.
fn work(kind: QueueKind, space: Option<SpaceId>, job: Option<&str>) -> Result<Work> {
    Ok(match (kind, space) {
        (QueueKind::Job, _) => {
            let job: QueuedJob = serde_json::from_str(job.context("a job's entry names no job")?)?;
            Work::Job {
                tier: job_tier(&job),
                spaces: job.space_ids,
            }
        }
        (QueueKind::Restore, Some(space)) => Work::Restore { space },
        (_, space) => Work::Launch { space },
    })
}

/// A job's own tier: finishing on a PR or the default branch, which work
/// close to done waits for, else new.
fn job_tier(job: &QueuedJob) -> Tier {
    match job.run.trigger {
        Trigger::Pull { .. } => Tier::Finishing,
        _ if job.default_branch => Tier::Finishing,
        _ => Tier::New,
    }
}
