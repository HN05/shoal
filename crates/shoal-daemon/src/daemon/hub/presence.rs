//! The hub's record of when each machine was there: the spans a member's
//! link stayed open, and the hub's own machine's daemon ran with the
//! machine awake, over the last [`KEPT`]. The hub marks each machine that is
//! there at once when its link opens and ends, and every minute meanwhile;
//! a mark less than [`SHORT_GAP`] after a span's end extends it, so gaps shorter
//! than that, such as a network blip or a daemon restart, never show.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::member_ids::MemberId;

use crate::time::UnixTime;

/// How long the hub keeps a span after it ended.
pub(super) const KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The shortest absence that ends a span.
pub(super) const SHORT_GAP: Duration = Duration::from_secs(5 * 60);

/// A time `member` was there, from `start` to `end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub start: UnixTime,
    pub end: UnixTime,
}

/// When the hub has seen `member` there: since when, none before it was,
/// and its spans of the last [`KEPT`], oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct History {
    pub since: Option<UnixTime>,
    pub spans: Vec<Span>,
}

/// Record that `member` is there at `now`: its latest span lasts until
/// `now` when it ended less than [`SHORT_GAP`] before, and a new one
/// starts otherwise.
pub(super) fn mark(tx: &Transaction<'_>, member: &MemberId, now: UnixTime) -> Result<()> {
    let extended = tx.execute(
        "UPDATE hub_presence SET ended_at=MAX(ended_at, ?2)
            WHERE member_id=?1 AND ended_at > ?3
              AND started_at=(SELECT MAX(started_at) FROM hub_presence WHERE member_id=?1)",
        params![member, now, now - SHORT_GAP],
    )?;
    if extended == 0 {
        tx.execute(
            "INSERT OR IGNORE INTO hub_presence(member_id, started_at, ended_at) VALUES (?1,?2,?2)",
            params![member, now],
        )?;
    }
    tx.execute(
        "UPDATE hub_members SET present_since=COALESCE(present_since, ?2) WHERE id=?1",
        params![member, now],
    )?;
    Ok(())
}

/// Forget the spans that ended more than [`KEPT`] before `now`.
pub(super) fn prune(tx: &Transaction<'_>, now: UnixTime) -> Result<()> {
    tx.execute("DELETE FROM hub_presence WHERE ended_at < ?1", [now - KEPT])?;
    Ok(())
}

/// When the hub has seen `member` there, with its spans that end within
/// [`KEPT`] of `now`.
pub(super) fn history(tx: &Transaction<'_>, member: &MemberId, now: UnixTime) -> Result<History> {
    let since = tx.query_row(
        "SELECT present_since FROM hub_members WHERE id=?1",
        [member],
        |row| row.get(0),
    )?;
    let mut statement = tx.prepare_cached(
        "SELECT started_at, ended_at FROM hub_presence
            WHERE member_id=?1 AND ended_at >= ?2 ORDER BY started_at",
    )?;
    let spans = statement.query_map(params![member, now - KEPT], |row| {
        Ok(Span {
            start: row.get(0)?,
            end: row.get(1)?,
        })
    })?;
    Ok(History {
        since,
        spans: spans.collect::<rusqlite::Result<_>>()?,
    })
}

#[cfg(test)]
mod tests;
