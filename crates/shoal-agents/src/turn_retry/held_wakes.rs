//! Updates held back from waking an agent whose turn retry waits for a
//! rate limit's known reset: a turn they started would fail as the last
//! one did, so they stay unread until the retry's message, or one from the
//! user, wakes the agent, and arrive with it.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;

use super::RESET_MARGIN;
use crate::{Agents, messages, state::TurnFailure, time::UnixTime};

impl Agents {
    /// Whether the agent of the space `id` gets no wake for its updates
    /// for now: its turn retry waits for a rate limit's known reset, and no
    /// alert from the user waits for it.
    pub async fn wakes_held(&self, id: &SpaceId) -> Result<bool> {
        let (id, now) = (id.to_owned(), UnixTime::now());
        self.store
            .read(move |db| {
                Ok(waits_for_reset(db, &id, now)? && !messages::user_alert_unread(db, &id)?)
            })
            .await
    }
}

/// Whether the space's turn retry, not delivered yet, waits at `now` for a
/// rate limit's known reset, until [`RESET_MARGIN`] past it; after that,
/// the limit has lifted, and an update may start a turn as the retry would.
fn waits_for_reset(db: &Connection, id: &SpaceId, now: UnixTime) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_turn_retries
            WHERE space_id=?1 AND delivered=0 AND failure=?2 AND resets_at>?3)",
        params![id, TurnFailure::RateLimited, now - RESET_MARGIN],
        |row| row.get(0),
    )?)
}
