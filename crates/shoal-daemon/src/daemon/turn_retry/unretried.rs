//! Telling of a failed turn Shoal does not retry: once per execution until
//! its agent ends a turn normally, so a run of failed turns adds one
//! notification rather than one each.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::{Manager, cancel};
use crate::{
    daemon::notifications::AlertKind,
    model::Space,
    state::{AgentState, TurnFailure},
};

/// Why Shoal does not retry a failed turn.
pub(super) enum Unretried {
    /// A retry does not fix the failure.
    Unfixable,
    /// The agent's tool has `retry` off.
    RetryOff,
}

impl Unretried {
    /// Why Shoal does not retry the turn `failure` ended for an agent whose
    /// tool `retries` or not, or `None` when it does.
    pub(super) fn of(failure: TurnFailure, retries: bool) -> Option<Self> {
        if !failure.retryable() {
            Some(Self::Unfixable)
        } else if !retries {
            Some(Self::RetryOff)
        } else {
            None
        }
    }

    fn message(&self, agent: &crate::agent::Agent, failure: TurnFailure) -> String {
        match self {
            Self::Unfixable => format!(
                "{agent}'s turn ended with an API error that a retry does not fix; check the agent"
            ),
            Self::RetryOff => format!(
                "{agent}'s turn ended with an API error ({}) and was not retried because \
                 `retry` is off; set `retry = true` under `[agents.{agent}]` to retry failed turns",
                failure.describe()
            ),
        }
    }
}

/// Drop the space's retry and note that `execution`'s turn `failure` ended
/// went unretried, together. Whether the note is new.
fn record(
    tx: &rusqlite::Transaction<'_>,
    space_id: &SpaceId,
    execution: &ExecutionId,
    failure: TurnFailure,
) -> Result<bool> {
    cancel(tx, space_id)?;
    let noted = tx.execute(
        "INSERT OR IGNORE INTO execution_unretried_turns(execution_id,failure) VALUES (?1,?2)",
        rusqlite::params![execution, failure],
    )?;
    Ok(noted > 0)
}

/// The failure that ended the unretried turn of `execution`, until its agent
/// ends a turn normally.
pub(in crate::daemon) fn failure(
    db: &Connection,
    execution: &ExecutionId,
) -> Result<Option<TurnFailure>> {
    Ok(db
        .query_row(
            "SELECT failure FROM execution_unretried_turns WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?)
}

/// Follow a report of `state` after `previous` from `execution`, for a turn
/// no failure ended: the agent going idle from a turn ended one that went
/// through, so the next failed turn is told again.
pub(in crate::daemon) fn follow_state(
    db: &Connection,
    execution: Option<&ExecutionId>,
    previous: Option<AgentState>,
    state: AgentState,
) -> Result<()> {
    let went_through =
        state == AgentState::Idle && previous.is_some_and(|previous| previous != state);
    if let (true, Some(execution)) = (went_through, execution) {
        db.execute(
            "DELETE FROM execution_unretried_turns WHERE execution_id=?1",
            [execution],
        )?;
    }
    Ok(())
}

impl Manager {
    /// Drop the space's retry and tell whoever receives `agent`'s events
    /// that Shoal does not retry the turn `failure` ended in `execution`,
    /// unless it told of a failed turn of the execution since the agent last
    /// ended one normally.
    pub(super) async fn leave_failed_turn(
        &self,
        space: &Space,
        execution: &ExecutionId,
        agent: &crate::agent::Agent,
        failure: TurnFailure,
        reason: Unretried,
    ) -> Result<()> {
        let (id, execution) = (space.id.clone(), execution.to_owned());
        if self
            .store
            .write(move |tx| record(tx, &id, &execution, failure))
            .await?
        {
            let message = reason.message(agent, failure);
            self.notify_space(&space.name, &space.id, AlertKind::TurnFailed, message)
                .await;
        }
        Ok(())
    }
}
