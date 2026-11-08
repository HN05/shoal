//! Review rows: queueing, the state changes of a run, and the answers to
//! findings, each in the caller's connection or transaction.
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{
    agent::ModelChoice,
    reviews::{
        Check, FailureReason, Finding, FindingId, Outcome, OutcomeKind, Posted, Reply, Review,
        ReviewId, ReviewState, ReviewStatus, ReviewTarget,
        output::{Checked, ReviewerOutput},
    },
};

/// What queueing a review found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Queued {
    /// A new review.
    New(ReviewId),
    /// The workspace's review that was already queued.
    Existing(ReviewId),
}

impl Queued {
    pub(crate) fn id(self) -> ReviewId {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }
}

/// Queue a review of the workspace, or return the one already queued; a
/// running review refuses another.
pub(crate) fn queue(
    db: &Connection,
    workspace_id: &SpaceId,
    target: &ReviewTarget,
    agent: &str,
    model: &ModelChoice,
    now: crate::time::UnixTime,
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
        "INSERT INTO reviews(workspace_id,target,agent,model,state,created_at)
            VALUES (?1,?2,?3,?4,?5,?6)",
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
pub(crate) fn active(
    db: &Connection,
    workspace_id: &str,
) -> Result<Option<(ReviewId, ReviewState)>> {
    Ok(db
        .query_row(
            "SELECT id,state FROM reviews WHERE workspace_id=?1 AND state IN (?2,?3)",
            params![workspace_id, ReviewState::Queued, ReviewState::Running],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// The queued or running reviews of the repository's workspaces.
pub(crate) fn active_in(db: &Connection, repository_id: &str) -> Result<Vec<ReviewId>> {
    Ok(db
        .prepare(
            "SELECT reviews.id FROM reviews JOIN workspaces ON workspaces.id=workspace_id
                WHERE repository_id=?1 AND reviews.state IN (?2,?3)",
        )?
        .query_map(
            params![repository_id, ReviewState::Queued, ReviewState::Running],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// Every queued review, oldest first.
pub(crate) fn queued(db: &Connection) -> Result<Vec<ReviewId>> {
    Ok(db
        .prepare("SELECT id FROM reviews WHERE state=?1 ORDER BY id")?
        .query_map([ReviewState::Queued], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Queue again the reviews a daemon stop interrupted, which start over,
/// except those in `kept`, whose runs could not be cleaned up.
pub(crate) fn requeue_running(db: &Connection, kept: &[ReviewId]) -> Result<usize> {
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
/// remove, and queue the other running reviews again; returns the reviews
/// it failed and how many it queued.
pub(crate) fn settle_interrupted(
    db: &Connection,
    kept: &[ReviewId],
    now: crate::time::UnixTime,
) -> Result<(Vec<ReviewId>, usize)> {
    let mut failed = Vec::new();
    for id in kept {
        if fail(db, *id, None, FailureReason::ExecutionFailed, now)? {
            failed.push(*id);
        }
    }
    Ok((failed, requeue_running(db, kept)?))
}

/// Queue running review `id` again after protection stopped its run, to wait
/// for healthy readings; false when it is no longer running.
pub(crate) fn requeue(db: &Connection, id: ReviewId) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?2,head=NULL,base=NULL,started_at=NULL,reviewer=NULL,
            waits_for_recovery=1
            WHERE id=?1 AND state=?3",
        params![id, ReviewState::Queued, ReviewState::Running],
    )? == 1)
}

/// Whether queued review `id` waits for healthy readings, as one protection
/// stopped does until it starts again.
pub(crate) fn waits_for_recovery(db: &Connection, id: ReviewId) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT waits_for_recovery FROM reviews WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// The reviews that are running.
pub(crate) fn running(db: &Connection) -> Result<Vec<ReviewId>> {
    Ok(db
        .prepare("SELECT id FROM reviews WHERE state=?1 ORDER BY id")?
        .query_map([ReviewState::Running], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Move a queued review to running at `head`; false when it is no longer
/// queued, as after a cancel.
pub(crate) fn start(
    db: &Connection,
    id: ReviewId,
    head: &str,
    base: &str,
    now: crate::time::UnixTime,
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

/// Record the reviewer's result on a running review, numbering its findings
/// from 1, and the checks it made of earlier `fixed` findings of the same
/// workspace. False when the review is no longer running.
pub(crate) fn finish(
    tx: &rusqlite::Transaction<'_>,
    id: ReviewId,
    reviewer: &str,
    output: &ReviewerOutput,
    now: crate::time::UnixTime,
) -> Result<bool> {
    let finished = tx.execute(
        "UPDATE reviews SET state=?6,reviewer=?2,summary=?3,verdict=?4,ended_at=?5
            WHERE id=?1 AND state=?7",
        params![
            id,
            reviewer,
            output.summary,
            output.verdict,
            now,
            ReviewState::Finished,
            ReviewState::Running
        ],
    )? == 1;
    if !finished {
        return Ok(false);
    }
    for (number, finding) in (1..).zip(&output.findings) {
        tx.execute(
            "INSERT INTO review_findings(review_id,number,severity,path,line,title,body)
                VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                number,
                finding.severity,
                finding.path,
                finding.line,
                finding.title,
                finding.body
            ],
        )?;
    }
    for checked in &output.checked {
        record_check(tx, id, checked)?;
    }
    Ok(true)
}

/// Record a check of an earlier finding of the same workspace that is
/// resolved `fixed`; checks of other findings are ignored.
fn record_check(db: &Connection, id: ReviewId, checked: &Checked) -> Result<()> {
    db.execute(
        "UPDATE review_findings SET check_result=?3,checked_by=?4
            WHERE review_id=?1 AND number=?2 AND json_extract(outcome,'$.outcome')=?5
            AND review_id IN (SELECT earlier.id FROM reviews earlier JOIN reviews this
                ON this.workspace_id=earlier.workspace_id
                WHERE this.id=?4 AND earlier.id<?4)",
        params![
            checked.finding.review,
            checked.finding.number,
            checked.result,
            id,
            OutcomeKind::Fixed
        ],
    )?;
    Ok(())
}

/// End an active review as failed for `reason`; false when it already ended.
pub(crate) fn fail(
    db: &Connection,
    id: ReviewId,
    reviewer: Option<&str>,
    reason: FailureReason,
    now: crate::time::UnixTime,
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
pub(crate) fn cancel(
    db: &Connection,
    id: ReviewId,
    by: Option<&SpaceId>,
    now: crate::time::UnixTime,
) -> Result<bool> {
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
pub(crate) fn cancelled_by(db: &Connection, id: ReviewId) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT cancelled_by FROM reviews WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub(crate) fn set_posted(db: &Connection, id: ReviewId, posted: &Posted) -> Result<()> {
    db.execute(
        "UPDATE reviews SET posted=?2 WHERE id=?1",
        params![id, serde_json::to_string(posted)?],
    )?;
    Ok(())
}

/// Add a reply to finding `id`.
pub(crate) fn reply(
    db: &Connection,
    id: FindingId,
    body: &str,
    now: crate::time::UnixTime,
) -> Result<()> {
    require_finding(db, id)?;
    db.execute(
        "INSERT INTO review_replies(review_id,number,created_at,body) VALUES (?1,?2,?3,?4)",
        params![id.review, id.number, now, body],
    )?;
    Ok(())
}

/// Resolve finding `id` with `outcome`, or reopen it without one. A
/// duplicate names another finding of the same workspace.
pub(crate) fn resolve(db: &Connection, id: FindingId, outcome: Option<&Outcome>) -> Result<()> {
    require_finding(db, id)?;
    if let Some(Outcome::Duplicate { of }) = outcome {
        ensure!(*of != id, "a finding cannot duplicate itself");
        let same_workspace: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_findings JOIN reviews ON reviews.id=review_id
                WHERE review_id=?1 AND number=?2 AND workspace_id=
                    (SELECT workspace_id FROM reviews WHERE id=?3))",
            params![of.review, of.number, id.review],
            |row| row.get(0),
        )?;
        ensure!(
            same_workspace,
            "finding {of} is not a finding of this workspace"
        );
    }
    let outcome = outcome.map(serde_json::to_string).transpose()?;
    db.execute(
        "UPDATE review_findings SET outcome=?3 WHERE review_id=?1 AND number=?2",
        params![id.review, id.number, outcome],
    )?;
    Ok(())
}

fn require_finding(db: &Connection, id: FindingId) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_findings WHERE review_id=?1 AND number=?2)",
        params![id.review, id.number],
        |row| row.get(0),
    )?;
    ensure!(
        exists,
        "finding {id} does not exist; shoal view review {} lists its findings",
        id.review
    );
    Ok(())
}

const COLUMNS: &str = "reviews.id,workspace_id,workspaces.name,target,agent,reviewer,reviews.state,
    failure,head,base,created_at,started_at,ended_at,summary,verdict,posted,reviews.model";

/// Review `id` with its findings.
pub(crate) fn load(db: &Connection, id: ReviewId) -> Result<Option<Review>> {
    load_record(db, id)?
        .map(|review| with_findings(db, review))
        .transpose()
}

/// Review `id` without its findings.
fn load_record(db: &Connection, id: ReviewId) -> Result<Option<Review>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM reviews JOIN workspaces ON workspaces.id=workspace_id
                    WHERE reviews.id=?1"
            ),
            [id],
            review,
        )
        .optional()?)
}

/// The reviews of the workspaces `ids`, newest first.
pub(crate) fn list(db: &Connection, ids: &[SpaceId]) -> Result<Vec<Review>> {
    let reviews = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM reviews JOIN workspaces ON workspaces.id=workspace_id
                WHERE workspace_id IN (SELECT value FROM json_each(?1)) ORDER BY reviews.id DESC"
        ))?
        .query_map([serde_json::to_string(ids)?], review)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    reviews
        .into_iter()
        .map(|review| with_findings(db, review))
        .collect()
}

/// The IDs of the repository's workspaces, or of every workspace.
pub(crate) fn workspaces(db: &Connection, repository: Option<&str>) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT id FROM workspaces WHERE ?1 IS NULL OR repository_id=?1")?
        .query_map([repository], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The workspace's newest finished review of commit `head`.
pub(crate) fn finished_at(
    db: &Connection,
    workspace_id: &SpaceId,
    head: &str,
) -> Result<Option<Review>> {
    let id: Option<ReviewId> = db.query_row(
        "SELECT MAX(id) FROM reviews WHERE workspace_id=?1 AND head=?2 AND state=?3",
        params![workspace_id, head, ReviewState::Finished],
        |row| row.get(0),
    )?;
    match id {
        Some(id) => load(db, id),
        None => Ok(None),
    }
}

/// The workspace's newest review.
pub(crate) fn latest(db: &Connection, workspace_id: &SpaceId) -> Result<Option<Review>> {
    match latest_id(db, workspace_id)? {
        Some(id) => load(db, id),
        None => Ok(None),
    }
}

/// The workspace's newest review as lists show it, which reads no replies.
pub(crate) fn latest_status(
    db: &Connection,
    workspace_id: &SpaceId,
) -> Result<Option<ReviewStatus>> {
    let Some(id) = latest_id(db, workspace_id)? else {
        return Ok(None);
    };
    let Some(mut review) = load_record(db, id)? else {
        return Ok(None);
    };
    review.findings = findings(db, id)?;
    Ok(Some((&review).into()))
}

fn latest_id(db: &Connection, workspace_id: &SpaceId) -> Result<Option<ReviewId>> {
    Ok(db.query_row(
        "SELECT MAX(id) FROM reviews WHERE workspace_id=?1",
        [workspace_id],
        |row| row.get(0),
    )?)
}

/// The workspace's reviews before `id` that returned findings, oldest
/// first: the history a new review receives.
pub(crate) fn earlier(
    db: &Connection,
    workspace_id: &SpaceId,
    id: ReviewId,
) -> Result<Vec<Review>> {
    let ids: Vec<ReviewId> = db
        .prepare("SELECT id FROM reviews WHERE workspace_id=?1 AND id<?2 AND state=?3 ORDER BY id")?
        .query_map(params![workspace_id, id, ReviewState::Finished], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    ids.into_iter()
        .filter_map(|id| load(db, id).transpose())
        .collect()
}

fn review(row: &Row) -> rusqlite::Result<Review> {
    let target: String = row.get(3)?;
    let posted: Option<String> = row.get(15)?;
    let model: Option<String> = row.get(16)?;
    Ok(Review {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        workspace: row.get(2)?,
        target: json(3, &target)?,
        agent: row.get(4)?,
        reviewer: row.get(5)?,
        state: row.get(6)?,
        failure: row.get(7)?,
        head: row.get(8)?,
        base: row.get(9)?,
        created_at: row.get(10)?,
        started_at: row.get(11)?,
        ended_at: row.get(12)?,
        summary: row.get(13)?,
        verdict: row.get(14)?,
        findings: Vec::new(),
        posted: posted
            .as_deref()
            .map(|posted| json(15, posted))
            .transpose()?,
        model: model
            .as_deref()
            .map(|model| json(16, model))
            .transpose()?
            .unwrap_or_default(),
        outdated: None,
    })
}

fn with_findings(db: &Connection, mut review: Review) -> Result<Review> {
    let mut findings = findings(db, review.id)?;
    add_replies(db, review.id, &mut findings)?;
    review.findings = findings;
    Ok(review)
}

/// Review `id`'s findings without their replies.
fn findings(db: &Connection, id: ReviewId) -> Result<Vec<Finding>> {
    db.prepare(
        "SELECT number,severity,path,line,title,body,outcome,check_result,checked_by
            FROM review_findings WHERE review_id=?1 ORDER BY number",
    )?
    .query_map([id], |row| {
        let outcome: Option<String> = row.get(6)?;
        let check = match (row.get(7)?, row.get(8)?) {
            (Some(result), Some(review)) => Some(Check { result, review }),
            _ => None,
        };
        Ok(Finding {
            id: FindingId {
                review: id,
                number: row.get(0)?,
            },
            severity: row.get(1)?,
            path: row.get(2)?,
            line: row.get(3)?,
            title: row.get(4)?,
            body: row.get(5)?,
            replies: Vec::new(),
            outcome: outcome.as_deref().map(|text| json(6, text)).transpose()?,
            check,
        })
    })?
    .collect::<rusqlite::Result<Vec<_>>>()
    .context("read review findings")
}

/// The replies to review `?1`'s findings, oldest first.
const REPLIES: &str =
    "SELECT number,created_at,body FROM review_replies WHERE review_id=?1 ORDER BY id";

/// Add the replies of review `id` to its `findings`.
fn add_replies(db: &Connection, id: ReviewId, findings: &mut [Finding]) -> Result<()> {
    let mut replies = db.prepare(REPLIES)?;
    for reply in replies.query_map([id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            Reply {
                created_at: row.get(1)?,
                body: row.get(2)?,
            },
        ))
    })? {
        let (number, reply) = reply?;
        match findings
            .iter_mut()
            .find(|finding| finding.id.number == number)
        {
            Some(finding) => finding.replies.push(reply),
            None => bail!("a reply names finding {id}.{number}, which is missing"),
        }
    }
    Ok(())
}

/// The JSON in column `index`, refusing what does not parse.
fn json<T: serde::de::DeserializeOwned>(index: usize, text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
    })
}

#[cfg(test)]
mod tests;
