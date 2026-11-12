//! The waiting entries as ranking sees them: the queue's waiting rows and
//! the one-shot runs waiting for a slot, in the order they arrived.
use std::collections::HashMap;

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

/// The waiting entries, the queue's and `oneshots`, numbered in the order
/// they arrived; `jobs` holds the spaces each waiting job serves now.
pub(super) fn waiting(
    db: &Connection,
    jobs: &HashMap<QueueEntryId, Vec<SpaceId>>,
    oneshots: &[WaitingOneShot],
) -> Result<Vec<Entry>> {
    let mut arrived = queued(db, jobs)?;
    arrived.extend(oneshots.iter().map(oneshot));
    // A one-shot run arrived after the entry it was queued after; the stable
    // sort keeps the order within each kind.
    arrived.sort_by_key(|(after, _)| *after);
    let entries = arrived.into_iter().map(|(_, entry)| entry);
    Ok(entries
        .enumerate()
        .map(|(arrival, entry)| Entry { arrival, ..entry })
        .collect())
}

/// When an entry arrived: after the queue entry of this sequence number, as
/// a one-shot run, or as that entry itself.
type Arrived = (i64, bool);

/// The queue's waiting entries in arrival order, each with when it arrived.
fn queued(
    db: &Connection,
    jobs: &HashMap<QueueEntryId, Vec<SpaceId>>,
) -> Result<Vec<(Arrived, Entry)>> {
    let rows = db
        .prepare(
            "SELECT seq,id,kind,name,created_at,COALESCE(space_id,opens_workspace_id),job
             FROM agent_queue WHERE state=?1 ORDER BY seq",
        )?
        .query_map([QueueState::Queued], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, QueueEntryId>(1)?,
                row.get::<_, QueueKind>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, UnixTime>(4)?,
                row.get::<_, Option<SpaceId>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut entries = Vec::new();
    for (seq, id, kind, name, queued_at, space, job) in rows {
        let work = work(kind, space, job.as_deref(), jobs.get(&id))?;
        let entry = Entry {
            id,
            name,
            work,
            queued_at,
            arrival: 0,
        };
        entries.push(((seq, false), entry));
    }
    Ok(entries)
}

/// The waiting review `oneshot`, with when it arrived: one that serves
/// nothing never waits, so it goes last.
fn oneshot(oneshot: &WaitingOneShot) -> (Arrived, Entry) {
    let serving = oneshot.serving.as_ref();
    let after = serving.map_or(i64::MAX, |serving| serving.queued_after);
    let entry = Entry {
        id: oneshot.id.clone(),
        name: "review".into(),
        work: Work::Review {
            space: serving.map(|serving| serving.space.clone()),
        },
        queued_at: oneshot.requested_at,
        arrival: 0,
    };
    ((after, true), entry)
}

/// What an entry of `kind` in `space` starts; `job` is a job's entry's job,
/// and `linked` the spaces it serves now, when known.
fn work(
    kind: QueueKind,
    space: Option<SpaceId>,
    job: Option<&str>,
    linked: Option<&Vec<SpaceId>>,
) -> Result<Work> {
    Ok(match (kind, space) {
        (QueueKind::Job, _) => {
            let job: QueuedJob = serde_json::from_str(job.context("a job's entry names no job")?)?;
            Work::Job {
                tier: job_tier(&job),
                spaces: linked.cloned().unwrap_or(job.space_ids),
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
