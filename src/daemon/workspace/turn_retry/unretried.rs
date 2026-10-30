//! Telling of a failed turn Shoal does not retry: once per execution until
//! its agent ends a turn normally, so a run of failed turns adds one
//! notification rather than one each.
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

use super::{Manager, cancel};
use crate::{
    daemon::notifications::NotificationKind,
    model::Workspace,
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

    fn message(&self, agent: &str, failure: TurnFailure) -> String {
        match self {
            Self::Unfixable => format!(
                "{agent}'s turn ended with an API error that a retry does not fix; check the agent"
            ),
            Self::RetryOff => format!(
                "{agent}'s turn ended with an API error ({}) and was not retried because \
                 `retry` is off; set `retry = true` under `[machine.ai.{agent}]` to retry failed turns",
                failure.describe()
            ),
        }
    }
}

/// Drop the workspace's retry and note that `execution`'s failed turn went
/// unretried, together. Whether the note is new.
fn record(db: &mut Connection, workspace_id: &str, execution: &str) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    cancel(&tx, workspace_id)?;
    let noted = tx.execute(
        "INSERT OR IGNORE INTO execution_unretried_turns(execution_id) VALUES (?1)",
        [execution],
    )?;
    tx.commit()?;
    Ok(noted > 0)
}

/// Follow a report of `state` after `previous` from `execution`, for a turn
/// no failure ended: the agent going idle from a turn ended one that went
/// through, so the next failed turn is told again.
pub(in crate::daemon::workspace) fn follow_state(
    db: &Connection,
    execution: Option<&str>,
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
    /// Drop the workspace's retry and tell whoever receives `agent`'s events
    /// that Shoal does not retry the turn `failure` ended in `execution`,
    /// unless it told of a failed turn of the execution since the agent last
    /// ended one normally.
    pub(super) async fn leave_failed_turn(
        &self,
        workspace: &Workspace,
        execution: &str,
        agent: &str,
        failure: TurnFailure,
        reason: Unretried,
    ) -> Result<()> {
        let (id, execution) = (workspace.id.clone(), execution.to_owned());
        if self
            .store
            .run(move |db| record(db, &id, &execution))
            .await?
        {
            let message = reason.message(agent, failure);
            self.notify(Some(&workspace.name), NotificationKind::TurnFailed, message)
                .await;
        }
        Ok(())
    }
}
