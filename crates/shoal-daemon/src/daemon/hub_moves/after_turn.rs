//! Moves to a joined machine that would stop the agent that asks for them,
//! as one of its own space does: Shoal records the move and starts it once
//! that agent's turn ends, as a conversion waits for it, acting with the
//! scope of its space, and tells the agent how a move that left its space
//! here went; one that took the space tells the agent there.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{alerts::AlertKind, log, manager::Manager, scope::Caller},
    protocol::operations::{Ended, Failure, OperationSpec, Outcome},
    time::UnixTime,
    transfer::{DeferredMove, MoveOrder},
};

impl Manager {
    /// Whether moving the spaces `order` takes stops the agent of
    /// `caller`'s space: that space is among them, or goes with one of
    /// them. A copy stops nothing.
    pub(crate) async fn move_stops_caller(
        &self,
        caller: &Caller,
        order: &MoveOrder,
    ) -> Result<bool> {
        if order.keep {
            return Ok(false);
        }
        let carrying = order.carrying();
        for id in &order.spaces {
            let carried = self.carried_by(id, &carrying).await?;
            let mut ended = std::iter::once(id).chain(carried.iter().map(|space| &space.id));
            if ended.any(|ended| *ended == caller.space_id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Record `order` to start once the turn of the agent of `caller`'s
    /// space ends, in place of a move it recorded before.
    pub(crate) async fn move_after_turn(
        &self,
        caller: &Caller,
        order: MoveOrder,
    ) -> Result<DeferredMove> {
        let (space, order) = (caller.space_id.clone(), serde_json::to_string(&order)?);
        let recorded = space.clone();
        (self.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO pending_moves(space_id,move_order,created_at) VALUES (?1,?2,?3)
                        ON CONFLICT(space_id) DO UPDATE SET move_order=excluded.move_order,
                            operation_id=NULL,created_at=excluded.created_at",
                    params![recorded, order, UnixTime::now()],
                )?;
                Ok(())
            })
            .await?;
        Ok(DeferredMove { space })
    }

    /// Start each recorded move whose agent's turn ended, acting with the
    /// scope of that agent's space.
    pub(crate) async fn start_moves_after_turns(&self) -> Result<()> {
        for (space, order) in self.store.read(waiting_moves).await? {
            if !self
                .turn_ended(&space, |agent| agent.space_id == space)
                .await?
            {
                continue;
            }
            let caller = Caller::of_space(space.clone());
            let first = order.spaces[0].clone();
            let spec = OperationSpec::Move {
                order: Box::new(order),
            };
            match self.spawn_operation(first, spec, None, Some(&caller)).await {
                Ok(id) => self.record_started(&space, &id).await?,
                Err(error) => {
                    self.forget_move_after_turn(&space).await;
                    let text = format!("The move of your space did not start: {error:#}");
                    self.tell_mover(&space, text).await;
                }
            }
        }
        Ok(())
    }

    /// Record that the operation `id` runs the move recorded for `space`,
    /// telling its agent how it went when it ended before this could.
    async fn record_started(&self, space: &SpaceId, id: &str) -> Result<()> {
        let (recorded, operation) = (space.clone(), id.to_owned());
        (self.store)
            .write(move |tx| started(tx, &recorded, &operation))
            .await?;
        let ending = (self.operation(id, None).await)
            .ok()
            .and_then(|entry| entry.ending());
        if let Some(ended) = ending {
            self.tell_move_after_turn(id, &ended).await;
        }
        Ok(())
    }

    /// Tell the agent whose move the operation `id` ran, once it `ended`
    /// with its space still here, how it went.
    pub(crate) async fn tell_move_after_turn(&self, id: &str, ended: &Ended) {
        // Taken in one write, so only one of those who read the end tells.
        let operation = id.to_owned();
        let space = (self.store).write(move |tx| take_move_of_operation(tx, &operation));
        let space = match space.await {
            Ok(Some(space)) => space,
            Ok(None) => return,
            Err(error) => {
                log!("the move of operation {id} is untold: {error:#}");
                return;
            }
        };
        self.tell_mover(&space, stayed_message(&space, ended)).await;
    }

    /// Settle each recorded move whose operation no longer runs: one that
    /// ended without telling, as when its worker went without reporting,
    /// tells its agent how it ended; one this daemon does not know, as one
    /// a restart cut off, which Shoal finishes as it finishes any
    /// interrupted move, tells its agent so.
    pub(crate) async fn forget_moves_cut_off(&self) -> Result<()> {
        for (space, operation) in self.store.read(started_moves).await? {
            if let Ok(entry) = self.operation(&operation, None).await {
                if let Some(ended) = entry.ending() {
                    self.tell_move_after_turn(&operation, &ended).await;
                }
                continue;
            }
            self.forget_move_after_turn(&space).await;
            let text = "Shoal restarted during the move of your space, which it finishes on its own; shoal status shows where your space is now";
            self.tell_mover(&space, text.to_owned()).await;
        }
        Ok(())
    }

    /// Forget the move recorded for the turn of the agent of `space`.
    async fn forget_move_after_turn(&self, space: &SpaceId) {
        let forgotten = space.clone();
        let forgetting = (self.store).write(move |tx| {
            tx.execute("DELETE FROM pending_moves WHERE space_id=?1", [&forgotten])?;
            Ok(())
        });
        if let Err(error) = forgetting.await {
            log!("forget the move of space {space} after its turn: {error:#}");
        }
    }

    /// Tell the agent of `space`, which asked for a move, `text`.
    async fn tell_mover(&self, space: &SpaceId, text: String) {
        let told = self.queue_agent_message(space, AlertKind::MoveFinished, text);
        if let Err(error) = told.await {
            log!("the move of space {space} is untold: {error:#}");
        }
    }
}

/// The moves recorded to start once their agents' turns end, by the space
/// of each agent.
fn waiting_moves(db: &rusqlite::Transaction<'_>) -> Result<Vec<(SpaceId, MoveOrder)>> {
    let mut statement =
        db.prepare("SELECT space_id,move_order FROM pending_moves WHERE operation_id IS NULL")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?;
    let mut moves = Vec::new();
    for row in rows {
        let (space, order) = row?;
        moves.push((space, serde_json::from_str(&order)?));
    }
    Ok(moves)
}

/// Forget the recorded move the operation `id` runs, if one does,
/// returning the space of its agent.
fn take_move_of_operation(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<Option<SpaceId>> {
    Ok(tx
        .query_row(
            "DELETE FROM pending_moves WHERE operation_id=?1 RETURNING space_id",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The recorded moves that started, by the space of each agent, with the
/// operation that runs each.
fn started_moves(db: &rusqlite::Transaction<'_>) -> Result<Vec<(SpaceId, String)>> {
    let mut statement = db.prepare(
        "SELECT space_id,operation_id FROM pending_moves WHERE operation_id IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record that the operation `id` runs the move recorded for `space`.
fn started(tx: &rusqlite::Transaction<'_>, space: &SpaceId, id: &str) -> Result<()> {
    tx.execute(
        "UPDATE pending_moves SET operation_id=?2 WHERE space_id=?1",
        params![space, id],
    )?;
    Ok(())
}

/// What the agent of `space`, whose move `ended` with its space still
/// here, hears of it.
fn stayed_message(space: &SpaceId, ended: &Ended) -> String {
    match ended {
        Ended::Succeeded(outcome) => match outcome.as_ref() {
            Outcome::Moved(report) => {
                // A space the move carried with one it took fails with that
                // one, which the report lists in its place.
                let own = report.failed.iter().find(|failed| failed.id == *space);
                match own.or(report.failed.first()) {
                    Some(failed) => format!("Your space did not move: {}", failed.error),
                    None => "Your space stayed on this machine while the spaces it moved went"
                        .to_owned(),
                }
            }
            _ => "Your space did not move".to_owned(),
        },
        Ended::Failed(Failure::Unconfirmed { question }) => format!(
            "Your space did not move: the move asks \"{question}\"; run it again with --yes to agree"
        ),
        Ended::Failed(failure) => format!(
            "Your space did not move: {}",
            failure.message().unwrap_or("its worker failed")
        ),
    }
}

#[cfg(test)]
mod tests;
