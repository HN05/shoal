//! The rows of reviews the user or a space writes: starting one, finding a
//! writer's pending reviews, and ending one, each in the caller's
//! connection or transaction.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{threads::Sender, time::UnixTime};

use shoal_refs::CommitId;

use super::Writer;
use crate::reviews::{ReviewId, ReviewState, ReviewTarget, Verdict};

/// The columns that name a writer: who writes, and the space that does.
fn columns(writer: &Writer) -> (Sender, Option<&SpaceId>) {
    match writer {
        Writer::User => (Sender::User, None),
        Writer::Space(id) => (Sender::Space, Some(id)),
    }
}

/// Start `writer`'s pending review of the workspace `workspace`, or find the
/// one it has.
pub fn start(
    db: &Connection,
    workspace: &SpaceId,
    writer: &Writer,
    now: UnixTime,
) -> Result<ReviewId> {
    if let Some(id) = pending_of(db, workspace, writer)? {
        return Ok(id);
    }
    let (opener, opener_id) = columns(writer);
    db.execute(
        "INSERT INTO reviews(workspace_id,target,opener,opener_id,state,created_at)
            VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            workspace,
            serde_json::to_string(&ReviewTarget::Workspace)?,
            opener,
            opener_id,
            ReviewState::Pending,
            now
        ],
    )?;
    Ok(ReviewId(db.last_insert_rowid()))
}

/// `writer`'s pending review of the workspace `workspace`.
pub fn pending_of(
    db: &Connection,
    workspace: &SpaceId,
    writer: &Writer,
) -> Result<Option<ReviewId>> {
    let (opener, opener_id) = columns(writer);
    Ok(db
        .query_row(
            "SELECT id FROM reviews WHERE workspace_id=?1 AND opener=?2 AND opener_id IS ?3
                AND state=?4",
            params![workspace, opener, opener_id, ReviewState::Pending],
            |row| row.get(0),
        )
        .optional()?)
}

/// `writer`'s pending reviews, oldest first.
pub fn pending_by(db: &Connection, writer: &Writer) -> Result<Vec<ReviewId>> {
    let (opener, opener_id) = columns(writer);
    Ok(db
        .prepare(
            "SELECT id FROM reviews WHERE opener=?1 AND opener_id IS ?2 AND state=?3 ORDER BY id",
        )?
        .query_map(params![opener, opener_id, ReviewState::Pending], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Who writes review `id`, unless Shoal's reviewer does.
pub fn writer(db: &Connection, id: ReviewId) -> Result<Option<Writer>> {
    let found: Option<(Sender, Option<SpaceId>)> = db
        .query_row(
            "SELECT opener,opener_id FROM reviews WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match found {
        Some((Sender::User, _)) => Some(Writer::User),
        Some((Sender::Space, Some(id))) => Some(Writer::Space(id)),
        _ => None,
    })
}

/// What submitting a review records: the commit it covers and the one that
/// commit is compared with, its summary and verdict.
pub struct Submitted<'a> {
    pub head: &'a CommitId,
    pub base: &'a CommitId,
    pub summary: Option<&'a str>,
    pub verdict: Verdict,
}

/// End pending review `id` as `submitted`, finished; false when it is no
/// longer pending.
pub fn submit(
    db: &Connection,
    id: ReviewId,
    submitted: &Submitted<'_>,
    now: UnixTime,
) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?2,head=?3,base=?4,summary=?5,verdict=?6,ended_at=?7
            WHERE id=?1 AND state=?8",
        params![
            id,
            ReviewState::Finished,
            submitted.head,
            submitted.base,
            submitted.summary,
            submitted.verdict,
            now,
            ReviewState::Pending
        ],
    )? == 1)
}

/// End pending review `id` as cancelled; false when it is no longer pending.
pub fn cancel(db: &Connection, id: ReviewId, now: UnixTime) -> Result<bool> {
    Ok(db.execute(
        "UPDATE reviews SET state=?2,ended_at=?3 WHERE id=?1 AND state=?4",
        params![id, ReviewState::Cancelled, now, ReviewState::Pending],
    )? == 1)
}
