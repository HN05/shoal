//! Updates held back from waking an agent whose turn retry waits for a
//! rate limit's known reset: a turn they started would fail as the last
//! one did, so they stay unread until the retry's message, or one from the
//! user, wakes the agent, and arrive with it.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;

use crate::{Agents, messages, state::TurnFailure};

impl Agents {
    /// Whether the agent of the space `id` gets no wake for its updates
    /// for now: its turn retry waits for a rate limit's known reset, and no
    /// alert from the user waits for it.
    pub async fn wakes_held(&self, id: &SpaceId) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .read(move |db| Ok(waits_for_reset(db, &id)? && !messages::user_alert_unread(db, &id)?))
            .await
    }
}

/// Whether the space's turn retry, not delivered yet, waits for a rate
/// limit's known reset.
fn waits_for_reset(db: &Connection, id: &SpaceId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_turn_retries
            WHERE space_id=?1 AND delivered=0 AND failure=?2 AND resets_at IS NOT NULL)",
        params![id, TurnFailure::RateLimited],
        |row| row.get(0),
    )?)
}
