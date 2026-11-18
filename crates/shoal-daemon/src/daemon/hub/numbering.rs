//! How this machine numbers its spaces while it takes part in a hub: from
//! the spares the hub keeps for it, which each report's answer brings, so
//! a new space never waits for the hub, and once none is left, above its
//! own highest number. A space whose number another machine's space held
//! first takes the number the hub gives it instead, with its questions and
//! approval requests, and the user and its agent hear of it. A space this
//! machine imports as another moves it here keeps the number it had there.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    hub::numbering::{Numbering, Renumbering},
    space_names::SpaceName,
};

use crate::daemon::{
    agent_messages::Delivery, approvals, log, notifications::NotificationKind, workspace::Manager,
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

    /// Keep `number`, which the hub keeps for the space `id` this machine
    /// imports, for when the import records the space here.
    pub(super) async fn keep_imported_number(
        &self,
        id: &SpaceId,
        number: SpaceNumber,
    ) -> Result<()> {
        let id = id.clone();
        (self.store)
            .write(move |tx| keep_imported(tx, &id, number))
            .await
    }

    /// The numbers this machine holds without a space, as for a removed
    /// space's issue proposals, which no other machine's space may take.
    pub(super) async fn reserved_numbers(&self) -> Result<Vec<SpaceNumber>> {
        self.store.read(reserved).await
    }

    /// Give new spaces the smallest number free here again, once this
    /// machine neither takes the hub role nor is a member of a hub.
    pub(super) async fn forget_numbering_unless_joined(&self) -> Result<()> {
        self.store.write(forget_unless_joined).await
    }
}

/// The number the new space `id` takes here from the hub's sequence: the
/// one it keeps as an import while no space here holds it, else the next
/// one; none while this machine takes no part in a hub's sequence, where it
/// takes the smallest free.
pub(crate) fn next(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
    if let Some(kept) = take_kept(tx, id)? {
        return Ok(Some(kept));
    }
    if !numbered_by_hub(tx)? {
        return Ok(None);
    }
    match free_spare(tx)? {
        Some(spare) => Ok(Some(spare)),
        None => above_highest(tx).map(Some),
    }
}

/// Whether this machine numbers its spaces from a hub's sequence.
fn numbered_by_hub(tx: &Transaction<'_>) -> Result<bool> {
    Ok(
        tx.query_row("SELECT EXISTS(SELECT 1 FROM hub_numbering)", [], |row| {
            row.get(0)
        })?,
    )
}

/// The smallest spare the hub keeps for this machine that no space here
/// holds.
fn free_spare(tx: &Transaction<'_>) -> Result<Option<SpaceNumber>> {
    Ok(tx.query_row(
        "SELECT MIN(spare.value) FROM hub_numbering, json_each(hub_numbering.spares) AS spare
            WHERE spare.value NOT IN (SELECT number FROM space_numbers)",
        [],
        |row| row.get(0),
    )?)
}

/// The number above the highest any space here holds.
fn above_highest(tx: &Transaction<'_>) -> Result<SpaceNumber> {
    Ok(tx.query_row(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM space_numbers",
        [],
        |row| row.get(0),
    )?)
}

/// See [`Manager::reserved_numbers`].
fn reserved(tx: &Transaction<'_>) -> Result<Vec<SpaceNumber>> {
    let mut statement = tx.prepare(
        "SELECT DISTINCT number FROM space_numbers
            WHERE number NOT IN (SELECT number FROM spaces WHERE number IS NOT NULL)
            ORDER BY number",
    )?;
    let numbers = statement.query_map([], |row| row.get(0))?;
    Ok(numbers.collect::<rusqlite::Result<_>>()?)
}

/// Keep `number` for the imported space `id`.
fn keep_imported(tx: &Transaction<'_>, id: &SpaceId, number: SpaceNumber) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO hub_kept_numbers(space_id, number) VALUES (?1,?2)",
        params![id, number],
    )?;
    Ok(())
}

/// The number the hub keeps for the imported space `id`, which this
/// spends, unless a space here holds it.
fn take_kept(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<SpaceNumber>> {
    let kept = tx
        .query_row(
            "DELETE FROM hub_kept_numbers WHERE space_id=?1 RETURNING number",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match kept {
        Some(number) if !held(tx, number)? => Ok(Some(number)),
        _ => Ok(None),
    }
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
/// its questions and approval requests, unless this machine holds that
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
        // The hub gives no number this machine reports it holds, so a space
        // recorded here since its last report holds this one.
        if held(tx, *number)? {
            log!("hub: space {space} keeps {from} while a space here holds {number}");
            continue;
        }
        give_number(tx, space, from, *number)?;
        done.push(Renumbered {
            id: space.clone(),
            name,
            from,
            to: *number,
        });
    }
    Ok(done)
}

/// Give the space `id` the number `to` instead of `from`, and its
/// questions and approval requests with it.
fn give_number(
    tx: &Transaction<'_>,
    id: &SpaceId,
    from: SpaceNumber,
    to: SpaceNumber,
) -> Result<()> {
    for update in [
        "UPDATE spaces SET number=?2 WHERE id=?1",
        "UPDATE questions SET space_number=?2 WHERE space_id=?1",
    ] {
        tx.execute(update, params![id, to])?;
    }
    approvals::renumber(tx, id, from, to)
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
