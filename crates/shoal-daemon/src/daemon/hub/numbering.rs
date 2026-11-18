//! How this machine numbers its spaces while it takes part in a hub: from
//! the spares the hub keeps for it, which each report's answer brings, so
//! a new space never waits for the hub, and once none is left, above its
//! own highest number. A space whose number another machine's space held
//! first takes the number the hub gives it instead, with its questions and
//! approval requests, and the user and its agent hear of it.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    hub::numbering::{Numbering, Renumbering},
    space_names::SpaceName,
};

use crate::daemon::{
    agent_messages::Delivery, log, notifications::NotificationKind, workspace::Manager,
};

/// A space here that took the number the hub gave it.
struct Renumbered {
    id: SpaceId,
    name: SpaceName,
    from: SpaceNumber,
    to: SpaceNumber,
}

impl Manager {
    /// Number this machine's spaces as `numbering`, which the hub answered
    /// a report with, says, telling the user and the agent of each space it
    /// renumbers.
    pub(super) async fn take_numbering(&self, numbering: Numbering) -> Result<()> {
        let renumbered = (self.store)
            .write(move |tx| {
                keep_spares(tx, &numbering)?;
                renumber(tx, &numbering.renumbered)
            })
            .await?;
        for space in renumbered {
            self.announce_renumbering(&space).await;
        }
        Ok(())
    }

    /// Tell the user and the agent of `space` its new number.
    async fn announce_renumbering(&self, space: &Renumbered) {
        let Renumbered { id, name, from, to } = space;
        let message = format!(
            "{name} is space {to} now, not {from}, which a space on another machine of the hub held first; its questions and approval requests are numbered {to}.N"
        );
        let kind = NotificationKind::SpaceRenumbered;
        self.notify_space(name, id, kind, message.clone()).await;
        let told = self.queue_agent_message_delivered(id, message, Delivery::NextTurn);
        if let Err(error) = told.await {
            log!("hub: could not tell space {id} its new number: {error:#}");
        }
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

/// Give each space `renumbered` names the number the hub gave it, with
/// its questions and approval requests, unless a space here holds that
/// number, and return those it renumbered.
fn renumber(tx: &Transaction<'_>, renumbered: &[Renumbering]) -> Result<Vec<Renumbered>> {
    let mut done = Vec::new();
    for Renumbering { space, number } in renumbered {
        let Some((name, from)) = space_number(tx, space)? else {
            continue;
        };
        if from == *number {
            continue;
        }
        if held(tx, *number)? {
            log!("hub: space {space} keeps {from}, since a space here holds {number}");
            continue;
        }
        give_number(tx, space, *number)?;
        done.push(Renumbered {
            id: space.clone(),
            name,
            from,
            to: *number,
        });
    }
    Ok(done)
}

/// Give the space `id` the number `number`, and its questions and approval
/// requests with it.
fn give_number(tx: &Transaction<'_>, id: &SpaceId, number: SpaceNumber) -> Result<()> {
    for update in [
        "UPDATE spaces SET number=?2 WHERE id=?1",
        "UPDATE questions SET space_number=?2 WHERE space_id=?1",
        "UPDATE access_requests SET space_number=?2 WHERE space_id=?1",
    ] {
        tx.execute(update, params![id, number])?;
    }
    Ok(())
}

/// The name and number of the space `id`, if it is here.
fn space_number(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<(SpaceName, SpaceNumber)>> {
    Ok(tx
        .query_row("SELECT name, number FROM spaces WHERE id=?1", [id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?)
}

/// Whether a space here, or a request that outlives its space, holds
/// `number`.
fn held(tx: &Transaction<'_>, number: SpaceNumber) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_numbers WHERE number=?1)",
        [number],
        |row| row.get(0),
    )?)
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
