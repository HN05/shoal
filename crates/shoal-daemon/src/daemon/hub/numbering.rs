//! How this machine numbers its spaces while it takes part in a hub: from
//! the spares the hub keeps for it, which each report's answer brings, so
//! a new space never waits for the hub, and once none is left, above its
//! own highest number, which the hub renumbers should another machine's
//! space hold it.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::hub::numbering::Numbering;

use crate::daemon::workspace::Manager;

impl Manager {
    /// Number this machine's new spaces from `numbering`, which the hub
    /// answered a report with.
    pub(super) async fn take_numbering(&self, numbering: Numbering) -> Result<()> {
        self.store
            .write(move |tx| keep_spares(tx, &numbering))
            .await
    }

    /// Give new spaces the smallest number free here again, once this
    /// machine neither takes the hub role nor is a member of a hub.
    pub(super) async fn forget_numbering_unless_joined(&self) -> Result<()> {
        self.store.write(forget_unless_joined).await
    }
}

/// The number a new space takes here from the hub's sequence; none while
/// this machine takes no part in one, where it takes the smallest free.
pub(crate) fn next(tx: &Transaction<'_>) -> Result<Option<SpaceNumber>> {
    Ok(tx
        .query_row(
            "SELECT COALESCE(
                (SELECT MIN(spare.value) FROM json_each(hub_numbering.spares) AS spare
                    WHERE spare.value NOT IN (SELECT number FROM space_numbers)),
                (SELECT COALESCE(MAX(number), 0) + 1 FROM space_numbers))
            FROM hub_numbering",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// Keep the spares `numbering` brings as those new spaces here take.
fn keep_spares(tx: &Transaction<'_>, numbering: &Numbering) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO hub_numbering(id, spares) VALUES (1, ?1)",
        [serde_json::to_string(&numbering.spares)?],
    )?;
    Ok(())
}

/// Stop numbering from a hub's sequence unless this machine is a member of
/// a hub that has not revoked it, as once it leaves one or gives up the hub
/// role.
pub(super) fn forget_unless_joined(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_numbering
            WHERE NOT EXISTS(SELECT 1 FROM hub_membership WHERE revoked=0)",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
