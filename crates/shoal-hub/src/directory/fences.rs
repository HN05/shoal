//! The moves through the hub that their source settled without the
//! destination's claim: each one's ticket refuses the claims that carry it
//! and come later, as from an import still running when the source gave
//! up on it, so the space never runs on both machines.
use std::time::Duration;

use anyhow::{Result, ensure};
use rusqlite::{Transaction, params};
use shoal_ids::{member_ids::MemberId, move_tickets::MoveTicket, space_ids::SpaceId};

use super::records;
use crate::time::UnixTime;

/// How long a settled move's ticket stays refused: far longer than an
/// import takes from its package's end to its claim.
const KEPT: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Refuse a claim that carries `ticket`, of a move its source settled
/// without it.
pub(super) fn ensure_unsettled(tx: &Transaction<'_>, ticket: &MoveTicket) -> Result<()> {
    let settled = shoal_store::exists(
        tx,
        "SELECT 1 FROM hub_move_fences WHERE ticket=?1",
        [ticket],
    )?;
    ensure!(
        !settled,
        "the machine the space moves from gave the move up, and the space stays there"
    );
    Ok(())
}

/// Settle the move `ticket` of the space `id` to `member`: whether
/// `member` claimed the space or owns it since its claim; when not, refuse
/// its claims that carry the ticket from now on.
pub(super) fn settle(
    tx: &Transaction<'_>,
    id: &SpaceId,
    member: &MemberId,
    ticket: &MoveTicket,
) -> Result<bool> {
    if records::claimed_by(tx, id, member)? || records::owner(tx, id)?.as_ref() == Some(member) {
        return Ok(true);
    }
    let now = UnixTime::now();
    tx.execute(
        "DELETE FROM hub_move_fences WHERE settled_at<?1",
        [now - KEPT],
    )?;
    tx.execute(
        "INSERT INTO hub_move_fences(ticket, settled_at) VALUES (?1,?2)
         ON CONFLICT(ticket) DO NOTHING",
        params![ticket, now],
    )?;
    Ok(false)
}

#[cfg(test)]
mod tests;
