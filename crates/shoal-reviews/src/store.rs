//! Review rows: queueing and the state changes of a run, whose reviewer
//! opens a thread for each finding and reopens the earlier ones still
//! present before it submits, each in the caller's connection or
//! transaction.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

use crate::{
    agent::{Agent, ModelChoice},
    daemon::Records,
    reviews::{
        Author, FailureReason, Posted, Review, ReviewId, ReviewState, ReviewStatus, ReviewTarget,
    },
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::threads::{Sender, ThreadSpace};
use shoal_protocol::{space_names::SpaceName, time::UnixTime};

/// What queueing a review found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    /// A new review.
    New(ReviewId),
    /// The workspace's review that was already queued.
    Existing(ReviewId),
}

impl Queued {
    pub fn id(self) -> ReviewId {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }
}

/// Queue a review of the workspace, or return the one already queued; a
/// running review refuses another.
pub fn queue(
    db: &Connection,
    workspace_id: &SpaceId,
    target: &ReviewTarget,
    agent: &Agent,
    model: &ModelChoice,
    now: UnixTime,
) -> Result<Queued> {
    if let Some((id, state)) = active(db, workspace_id)? {
        ensure!(
            state == ReviewState::Queued,
            "review {id} of this workspace is running; wait for it with shoal watch review {id} \
             or stop it with shoal review cancel {id}"
        );
        return Ok(Queued::Existing(id));
    }
    db.execute(
        "INSERT INTO reviews(workspace_id,target,opener,agent,model,state,created_at)
            VALUES (?1,?2,'shoal',?3,?4,?5,?6)",
        params![
            workspace_id,
            serde_json::to_string(target)?,
            agent,
            (!model.is_empty())
                .then(|| serde_json::to_string(model))
                .transpose()?,
            ReviewState::Queued,
            now
        ],
    )?;
    Ok(Queued::New(ReviewId(db.last_insert_rowid())))
}

/// The workspace's queued or running review.
pub fn active(db: &Connection, workspace_id: &SpaceId) -> Result<Option<(ReviewId, ReviewState)>> {
    Ok(db
        .query_row(
            "SELECT id,state FROM reviews WHERE workspace_id=?1 AND state IN (?2,?3)",
            params![workspace_id, ReviewState::Queued, ReviewState::Running],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// The queued or running reviews of the repository's workspaces.
pub fn active_in(
    db: &Connection,
    records: &dyn Records,
    repository_id: &RepositoryId,
) -> Result<Vec<ReviewId>> {
    let workspaces = records.workspaces(db, Some(repository_id))?;
    Ok(db
        .prepare(
            "SELECT id FROM reviews WHERE workspace_id IN (SELECT value FROM json_each(?1))
                AND state IN (?2,?3)",
        )?
        .query_map(
            params![
                serde_json::to_string(&workspaces)?,
                ReviewState::Queued,
                ReviewState::Running
            ],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Every queued review, oldest first.
pub fn queued(db: &Connection) -> Result<Vec<ReviewId>> {
    Ok(db
        .prepare("SELECT id FROM reviews WHERE state=?1 ORDER BY id")?
        .query_map([ReviewState::Queued], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Queue again the reviews a daemon stop interrupted, which start over,
/// except those in `kept`, whose runs could not be cleaned up.
pub fn requeue_running(db: &Connection, kept: &[ReviewId]) -> Result<usize> {
    Ok(db.execute(
        "UPDATE reviews SET state=?2,head=NULL,base=NULL,started_at=NULL,reviewer=NULL
            WHERE state=?3 AND id NOT IN (SELECT value FROM json_each(?1))",
        params![
            serde_json::to_string(kept)?,
            ReviewState::Queued,
            ReviewState::Running
        ],
    )?)
}

/// Fail the active reviews among `kept`, whose runs a restart could not
/// remove, and queue the other running reviews again, dropping what each
/// running review's run wrote; returns the reviews it failed and how many
/// it queued.
pub fn settle_interrupted(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    kept: &[ReviewId],
    now: UnixTime,
) -> Result<(Vec<ReviewId>, usize)> {
    for id in running(tx)? {
        drop_run_writes(tx, records, id)?;
    }
    let mut failed = Vec::new();
    for id in kept {
        if fail(tx, *id, None, FailureReason::ExecutionFailed, now)? {
            failed.push(*id);
        }
    }
    Ok((failed, requeue_running(tx, kept)?))
}

/// Queue running review `id` again after its run was stopped, by
/// protection to wait for healthy readings when `waits_for_recovery`, else
/// by the machine's sleep; false when it is no longer running.
pub fn requeue(db: &Connection, id: ReviewId, waits_for_recovery: bool) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?2,head=NULL,base=NULL,started_at=NULL,reviewer=NULL,
            waits_for_recovery=?4
            WHERE id=?1 AND state=?3",
        params![
            id,
            ReviewState::Queued,
            ReviewState::Running,
            waits_for_recovery
        ],
    )? == 1)
}

/// Whether queued review `id` waits for healthy readings, as one protection
/// stopped does until it starts again.
pub fn waits_for_recovery(db: &Connection, id: ReviewId) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT waits_for_recovery FROM reviews WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// The state of review `id`, none once it is gone.
pub fn state(db: &Connection, id: ReviewId) -> Result<Option<ReviewState>> {
    Ok(db
        .query_row("SELECT state FROM reviews WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?)
}

/// The reviews that are running.
pub fn running(db: &Connection) -> Result<Vec<ReviewId>> {
    Ok(db
        .prepare("SELECT id FROM reviews WHERE state=?1 ORDER BY id")?
        .query_map([ReviewState::Running], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Move a queued review to running at `head`; false when it is no longer
/// queued, as after a cancel.
pub fn start(
    db: &Connection,
    id: ReviewId,
    head: &CommitId,
    base: &CommitId,
    now: UnixTime,
) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?5,head=?2,base=?3,started_at=?4,waits_for_recovery=0
            WHERE id=?1 AND state=?6",
        params![
            id,
            head,
            base,
            now,
            ReviewState::Running,
            ReviewState::Queued
        ],
    )? == 1)
}

/// Record that `agent` reviews running review `id` now.
pub fn attempt(db: &Connection, id: ReviewId, agent: &Agent) -> Result<()> {
    db.execute(
        "UPDATE reviews SET reviewer=?2 WHERE id=?1 AND state=?3",
        params![id, agent, ReviewState::Running],
    )?;
    Ok(())
}

/// Hold, until running review `id` is submitted, its reopen of the finding
/// whose thread is `thread`, with the `reason` its post gives.
pub fn hold_reopen(db: &Connection, id: ReviewId, thread: i64, reason: &str) -> Result<()> {
    db.execute(
        "INSERT INTO review_reopens(review_id,thread_id,reason) VALUES (?1,?2,?3)
            ON CONFLICT(review_id,thread_id) DO UPDATE SET reason=excluded.reason",
        params![id, thread, reason],
    )?;
    Ok(())
}

/// Finish running review `id` as its reviewer submitted it, with `summary`
/// and the verdict its threads give, at the commits its run recorded:
/// post the reopens it held and let its threads post as any thread does.
/// False when it is no longer running.
pub fn submit_run(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    id: ReviewId,
    workspace: &SpaceId,
    summary: Option<&str>,
    approve: bool,
    now: UnixTime,
) -> Result<bool> {
    let comments = records.review_threads(tx, id)?.len();
    let verdict = crate::written::verdict(approve, comments);
    let submitted = tx.execute(
        "UPDATE reviews SET state=?2,summary=?3,verdict=?4,ended_at=?5 WHERE id=?1 AND state=?6",
        params![
            id,
            ReviewState::Finished,
            summary,
            verdict,
            now,
            ReviewState::Running
        ],
    )? == 1;
    if !submitted {
        return Ok(false);
    }
    post_held_reopens(tx, records, id, workspace)?;
    records.release_review_threads(tx, id)?;
    Ok(true)
}

/// Post the reopens review `id` held in the threads of the workspace
/// `workspace`, dropping each whose thread is no longer a finding resolved
/// `fixed`.
fn post_held_reopens(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    id: ReviewId,
    workspace: &SpaceId,
) -> Result<()> {
    let held: Vec<(i64, String)> = tx
        .prepare(
            "SELECT thread_id,reason FROM review_reopens WHERE review_id=?1 AND reason IS NOT NULL",
        )?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (thread, reason) in held {
        let sql = match records.reopen_finding(tx, workspace, thread, &reason)? {
            true => "UPDATE review_reopens SET reason=NULL WHERE review_id=?1 AND thread_id=?2",
            false => "DELETE FROM review_reopens WHERE review_id=?1 AND thread_id=?2",
        };
        tx.execute(sql, params![id, thread])?;
    }
    Ok(())
}

/// Drop what the run of Shoal's review `id` wrote while the review has not
/// finished: the threads it opened and the reopens it held, before the
/// review fails, its fallback runs or it runs again.
pub fn drop_run_writes(
    tx: &rusqlite::Transaction<'_>,
    records: &dyn Records,
    id: ReviewId,
) -> Result<()> {
    let unfinished: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM reviews WHERE id=?1 AND opener='shoal' AND state<>?2)",
        params![id, ReviewState::Finished],
        |row| row.get(0),
    )?;
    if unfinished {
        records.drop_review_threads(tx, id)?;
        tx.execute("DELETE FROM review_reopens WHERE review_id=?1", [id])?;
    }
    Ok(())
}

/// The session the latest earlier review of review `id`'s workspace and PR,
/// or of the workspace alone, recorded, with the agent that reported it.
pub fn session(db: &Connection, id: ReviewId) -> Result<Option<(Agent, AgentSessionId)>> {
    let found = subject_session(db, id, "earlier.id<?1")?;
    Ok(found.map(|(_, agent, session)| (agent, session)))
}

/// The session the latest review of review `id`'s workspace and PR, or of
/// the workspace alone, this one included, recorded: the review that holds
/// it, and the agent that reported it.
pub fn latest_session(
    db: &Connection,
    id: ReviewId,
) -> Result<Option<(ReviewId, Agent, AgentSessionId)>> {
    subject_session(db, id, "TRUE")
}

/// The session the latest review of review `id`'s subject that `which`
/// keeps recorded, with that review and the agent that reported it.
fn subject_session(
    db: &Connection,
    id: ReviewId,
    which: &str,
) -> Result<Option<(ReviewId, Agent, AgentSessionId)>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT earlier.id,earlier.reviewer,earlier.session FROM reviews earlier
                    JOIN reviews this ON this.workspace_id=earlier.workspace_id
                    AND json_extract(this.target,'$.url') IS json_extract(earlier.target,'$.url')
                    WHERE this.id=?1 AND {which} AND earlier.session IS NOT NULL
                    ORDER BY earlier.id DESC LIMIT 1"
            ),
            [id],
            |row| {
                let session = AgentSessionId::from(row.get::<_, String>(2)?);
                Ok((row.get(0)?, row.get(1)?, session))
            },
        )
        .optional()?)
}

/// Record that review `id`'s session went on as `session`, which later
/// reviews of its subject continue.
pub fn continue_session(db: &Connection, id: ReviewId, session: &AgentSessionId) -> Result<()> {
    db.execute(
        "UPDATE reviews SET session=?2 WHERE id=?1",
        params![id, session.as_str()],
    )?;
    Ok(())
}

/// End an active review as failed for `reason`; false when it already ended.
pub fn fail(
    db: &Connection,
    id: ReviewId,
    reviewer: Option<&Agent>,
    reason: FailureReason,
    now: UnixTime,
) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?5,failure=?2,reviewer=?3,ended_at=?4
            WHERE id=?1 AND state IN (?6,?7)",
        params![
            id,
            reason,
            reviewer,
            now,
            ReviewState::Failed,
            ReviewState::Queued,
            ReviewState::Running
        ],
    )? == 1)
}

/// Cancel an active review for a process of workspace `by`, if one asked;
/// false when it already ended.
pub fn cancel(db: &Connection, id: ReviewId, by: Option<&SpaceId>, now: UnixTime) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?4,cancelled_by=?2,ended_at=?3
            WHERE id=?1 AND state IN (?5,?6)",
        params![
            id,
            by,
            now,
            ReviewState::Cancelled,
            ReviewState::Queued,
            ReviewState::Running
        ],
    )? == 1)
}

/// The workspace whose process cancelled the review.
pub fn cancelled_by(db: &Connection, id: ReviewId) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT cancelled_by FROM reviews WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub fn set_posted(db: &Connection, id: ReviewId, posted: &Posted) -> Result<()> {
    db.execute(
        "UPDATE reviews SET posted=?2 WHERE id=?1",
        params![id, serde_json::to_string(posted)?],
    )?;
    Ok(())
}

const COLUMNS: &str = "id,workspace_id,target,agent,reviewer,state,failure,head,base,created_at,
    started_at,ended_at,summary,verdict,posted,model,opener,opener_id";

/// Review `id` with its threads.
pub fn load(db: &Connection, records: &dyn Records, id: ReviewId) -> Result<Option<Review>> {
    load_record(db, records, id)?
        .map(|review| with_threads(db, records, review))
        .transpose()
}

/// Review `id` without its threads, while its workspace exists.
fn load_record(db: &Connection, records: &dyn Records, id: ReviewId) -> Result<Option<Review>> {
    let review = db
        .query_row(
            &format!("SELECT {COLUMNS} FROM reviews WHERE id=?1"),
            [id],
            review,
        )
        .optional()?;
    Ok(review
        .map(|review| named(db, records, review))
        .transpose()?
        .flatten())
}

/// `review` with its workspace's name, and the number and name of the
/// space that wrote it, or none once the workspace is gone.
fn named(db: &Connection, records: &dyn Records, mut review: Review) -> Result<Option<Review>> {
    let Some(name) = records.space_name(db, &review.workspace_id)? else {
        return Ok(None);
    };
    review.workspace = name;
    if let Author::Space { space } = &mut review.author {
        *space = records.thread_space(db, &space.id)?;
    }
    Ok(Some(review))
}

/// The reviews of the workspaces `ids`, newest first.
pub fn list(db: &Connection, records: &dyn Records, ids: &[SpaceId]) -> Result<Vec<Review>> {
    let reviews = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM reviews
                WHERE workspace_id IN (SELECT value FROM json_each(?1)) ORDER BY id DESC"
        ))?
        .query_map([serde_json::to_string(ids)?], review)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut listed = Vec::new();
    for review in reviews {
        if let Some(review) = named(db, records, review)? {
            listed.push(with_threads(db, records, review)?);
        }
    }
    Ok(listed)
}

/// The finished reviews of commit `head`, oldest first: until each
/// resolves its threads, no other review of the commit opens a gate.
pub fn finished_all_at(
    db: &Connection,
    records: &dyn Records,
    workspace_id: &SpaceId,
    head: &CommitId,
) -> Result<Vec<Review>> {
    let ids: Vec<ReviewId> = db
        .prepare(
            "SELECT id FROM reviews WHERE workspace_id=?1 AND head=?2 AND state=?3 ORDER BY id",
        )?
        .query_map(params![workspace_id, head, ReviewState::Finished], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    ids.into_iter()
        .filter_map(|id| load(db, records, id).transpose())
        .collect()
}

/// The workspace's newest finished review of commit `head` that judges it:
/// any of Shoal's, or one its writer submitted with comments or approving
/// the work, so a review that only comments never decides a gate.
pub fn finished_at(
    db: &Connection,
    records: &dyn Records,
    workspace_id: &SpaceId,
    head: &CommitId,
) -> Result<Option<Review>> {
    let id: Option<ReviewId> = db.query_row(
        "SELECT MAX(id) FROM reviews WHERE workspace_id=?1 AND head=?2 AND state=?3
            AND (opener='shoal' OR verdict IS NOT 'comment')",
        params![workspace_id, head, ReviewState::Finished],
        |row| row.get(0),
    )?;
    match id {
        Some(id) => load(db, records, id),
        None => Ok(None),
    }
}

/// The workspace's newest review but a pending one.
pub fn latest(
    db: &Connection,
    records: &dyn Records,
    workspace_id: &SpaceId,
) -> Result<Option<Review>> {
    match latest_id(db, workspace_id)? {
        Some(id) => load(db, records, id),
        None => Ok(None),
    }
}

/// The workspace's newest review but a pending one, as lists show it.
pub fn latest_status(
    db: &Connection,
    records: &dyn Records,
    workspace_id: &SpaceId,
) -> Result<Option<ReviewStatus>> {
    let review = latest(db, records, workspace_id)?;
    Ok(review.as_ref().map(ReviewStatus::from))
}

/// The workspace's newest review but a pending one, which is its writer's
/// until submitted.
fn latest_id(db: &Connection, workspace_id: &SpaceId) -> Result<Option<ReviewId>> {
    Ok(db.query_row(
        "SELECT MAX(id) FROM reviews WHERE workspace_id=?1 AND state IS NOT ?2",
        params![workspace_id, ReviewState::Pending],
        |row| row.get(0),
    )?)
}

/// The workspace's reviews by Shoal's reviewer before `id` that returned
/// findings, oldest first: the history a new review receives.
pub fn earlier(
    db: &Connection,
    records: &dyn Records,
    workspace_id: &SpaceId,
    id: ReviewId,
) -> Result<Vec<Review>> {
    let ids: Vec<ReviewId> = db
        .prepare(
            "SELECT id FROM reviews WHERE workspace_id=?1 AND id<?2 AND state=?3
                AND opener='shoal' ORDER BY id",
        )?
        .query_map(params![workspace_id, id, ReviewState::Finished], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    ids.into_iter()
        .filter_map(|id| load(db, records, id).transpose())
        .collect()
}

fn review(row: &Row) -> rusqlite::Result<Review> {
    let target: String = row.get(2)?;
    let posted: Option<String> = row.get(14)?;
    Ok(Review {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        // Read from the spaces' records by [`named`].
        workspace: SpaceName::from(String::new()),
        target: json(2, &target)?,
        author: author(row)?,
        reviewer: row.get(4)?,
        state: row.get(5)?,
        failure: row.get(6)?,
        head: row.get(7)?,
        base: row.get(8)?,
        created_at: row.get(9)?,
        started_at: row.get(10)?,
        ended_at: row.get(11)?,
        summary: row.get(12)?,
        verdict: row.get(13)?,
        threads: Vec::new(),
        reopened: Vec::new(),
        posted: posted
            .as_deref()
            .map(|posted| json(14, posted))
            .transpose()?,
        outdated: None,
    })
}

/// Who wrote the review in `row`: Shoal, with the agent and model its
/// reviewer runs, the user, or a space, which [`named`] names.
fn author(row: &Row) -> rusqlite::Result<Author> {
    Ok(match row.get(16)? {
        Sender::Shoal => {
            let model: Option<String> = row.get(15)?;
            Author::Shoal {
                agent: row.get(3)?,
                model: (model.as_deref())
                    .map(|model| json(15, model))
                    .transpose()?
                    .unwrap_or_default(),
            }
        }
        Sender::User => Author::User,
        Sender::Space => Author::Space {
            space: ThreadSpace::here(row.get(17)?),
        },
    })
}

/// `review` with the threads it opened and those it reopened.
fn with_threads(db: &Connection, records: &dyn Records, mut review: Review) -> Result<Review> {
    review.threads = records.review_threads(db, review.id)?;
    let reopened: Vec<i64> = db
        .prepare("SELECT thread_id FROM review_reopens WHERE review_id=?1 ORDER BY rowid")?
        .query_map([review.id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    review.reopened = records.threads(db, &reopened)?;
    Ok(review)
}

/// The JSON in column `index`, refusing what does not parse.
fn json<T: serde::de::DeserializeOwned>(index: usize, text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
    })
}

#[cfg(test)]
mod tests;
