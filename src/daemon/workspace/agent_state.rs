//! The turn state agents report from their hooks: a status signal for
//! integrations that never notifies, completes, or changes cleanup.
use std::time::Duration;

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::{
    execution::recovery,
    model::{AgentStatus, Execution},
    process::{execution::Processes, identity::Scan},
    protocol::TurnEnd,
    state::{AgentState, ExecutionState, TurnFailure},
};

pub(crate) fn get(db: &Connection, id: &str) -> Result<Option<AgentStatus>> {
    Ok(db
        .query_row(
            "SELECT state,since,prompt_cache_expires_at FROM workspace_agent_state
             WHERE workspace_id=?1",
            [id],
            status,
        )
        .optional()?)
}

/// The last reported state while an agent can still be in it: one reported
/// from outside a tracked execution, or from one that runs. An `unknown`
/// execution may have died with its wrapper or may reattach after a daemon
/// restart, so its state stays recorded and shows again once it runs.
pub(crate) fn current(db: &Connection, id: &str) -> Result<Option<AgentStatus>> {
    Ok(db
        .query_row(
            "SELECT s.state,s.since,s.prompt_cache_expires_at FROM workspace_agent_state s
             LEFT JOIN executions e ON e.id=s.execution_id
             WHERE s.workspace_id=?1 AND (s.execution_id IS NULL OR e.state=?2)",
            params![id, ExecutionState::Running],
            status,
        )
        .optional()?)
}

fn status(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentStatus> {
    Ok(AgentStatus {
        state: row.get(0)?,
        since: row.get(1)?,
        prompt_cache_expires_at: row.get(2)?,
    })
}

/// Every workspace's last reported state.
pub(crate) fn all(db: &Connection) -> Result<Vec<(String, AgentState)>> {
    Ok(db
        .prepare("SELECT workspace_id,state FROM workspace_agent_state")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The turn a stopped execution last reported, while its record lasts.
struct StoppedTurn {
    workspace: String,
    state: AgentState,
    execution: Option<Execution>,
}

impl StoppedTurn {
    /// The turn to keep: an idle one counts as working while `scan` finds
    /// work of the execution outside its process group, as protection counts
    /// it, since that work may start its next turn.
    fn kept(&self, scan: Option<&Scan>) -> AgentState {
        let in_flight = || {
            self.execution
                .as_ref()
                .zip(scan)
                .is_some_and(|(execution, scan)| Processes::runs_outside_group(execution, scan))
        };
        if self.state == AgentState::Idle && in_flight() {
            AgentState::Working
        } else {
            self.state
        }
    }
}

fn reported_by(db: &Connection, id: &str) -> Result<Option<StoppedTurn>> {
    let reported: Option<(String, AgentState)> = db
        .query_row(
            "SELECT workspace_id,state FROM workspace_agent_state WHERE execution_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((workspace, state)) = reported else {
        return Ok(None);
    };
    Ok(Some(StoppedTurn {
        workspace,
        state,
        execution: crate::daemon::store::find_execution(db, id)?,
    }))
}

impl Manager {
    /// Keep the state execution `id` last reported in the recovery record it
    /// saved, before the state goes with the execution's record, judged by
    /// `scan` when its processes were scanned. A failure leaves the turn
    /// unknown, as in records saved before turns were kept.
    pub(crate) async fn keep_stopped_turn(&self, id: &str, scan: Option<&Scan>) {
        let query = id.to_owned();
        let kept = async {
            let reported = self.store.run(move |db| reported_by(db, &query)).await?;
            let Some(turn) = reported else {
                return Ok(());
            };
            recovery::save_turn(&self.paths, &turn.workspace, id, turn.kept(scan))
        }
        .await;
        if let Err(error) = kept {
            crate::daemon::log!("turn state of stopped execution {id} not kept: {error:#}");
        }
    }

    /// Record the state reported from `execution`, which clears it when its
    /// record goes, and the agent `session` it reports for resuming it.
    /// Repeating the current report writes nothing, since hooks repeat it
    /// after every tool call, and a repeated state keeps its start.
    pub async fn set_agent_state(
        &self,
        selector: &str,
        state: AgentState,
        execution: Option<String>,
        session: Option<String>,
    ) -> Result<AgentStatus> {
        self.record_agent_state(selector, state, execution, session, TurnEnding::default())
            .await
    }

    /// Record that the agent reporting from `execution` ended a turn and is
    /// idle, its prompt cache expiring the lifetime of its AI tool from now.
    pub async fn end_agent_turn(
        &self,
        selector: &str,
        execution: Option<String>,
        session: Option<String>,
        turn_end: TurnEnd,
    ) -> Result<AgentStatus> {
        let ending = TurnEnding {
            prompt_cache: Some(self.prompt_cache(execution.as_deref(), turn_end).await),
            ..TurnEnding::ended()
        };
        self.record_agent_state(selector, AgentState::Idle, execution, session, ending)
            .await
    }

    /// Record the turn a wake hook starts for the agent `execution` runs,
    /// unless a stop of the agent came first: the hook then keeps the update
    /// for the agent's restore. Whether the turn starts.
    pub(crate) async fn start_woken_turn(
        &self,
        selector: &str,
        execution: Option<String>,
    ) -> Result<bool> {
        if let Some(id) = &execution
            && !self.claim_woken_turn(id).await
        {
            return Ok(false);
        }
        self.record_agent_state(
            selector,
            AgentState::Working,
            execution,
            None,
            TurnEnding::default(),
        )
        .await?;
        Ok(true)
    }

    /// Record that an interrupt ended the turn of the agent `execution`
    /// runs, which leaves it idle without the hook that ends its other
    /// turns.
    pub(crate) async fn end_interrupted_turn(
        &self,
        selector: &str,
        execution: String,
    ) -> Result<AgentStatus> {
        let ending = TurnEnding {
            interrupted: true,
            prompt_cache: Some(
                self.prompt_cache(Some(&execution), TurnEnd::default())
                    .await,
            ),
            ..TurnEnding::ended()
        };
        self.record_agent_state(selector, AgentState::Idle, Some(execution), None, ending)
            .await
    }

    /// How long the prompt cache of the agent `execution` runs lives after
    /// the turn `turn_end` reports: as its AI tool resolves it, or as
    /// reported for an agent without one, else the default lifetime.
    async fn prompt_cache(&self, execution: Option<&str>, turn_end: TurnEnd) -> Duration {
        let reported = turn_end.prompt_cache_secs.map(Duration::from_secs);
        let agent = match execution {
            Some(execution) => self.agent_name(execution).await,
            None => None,
        };
        let config = self.config();
        match agent.and_then(|agent| config.machine.ai.tools.get(&agent)) {
            Some(tool) => tool.prompt_cache(reported),
            None => reported.unwrap_or(crate::ai::DEFAULT_PROMPT_CACHE),
        }
    }

    /// Record `state` as [`Self::set_agent_state`] does, for a report that
    /// `ending` says how it ends the agent's turn.
    async fn record_agent_state(
        &self,
        selector: &str,
        state: AgentState,
        execution: Option<String>,
        session: Option<String>,
        ending: TurnEnding,
    ) -> Result<AgentStatus> {
        let id = self.workspace(selector).await?.id;
        if let Some(execution) = &execution {
            self.record_agent_turn(execution, state, ending.interrupted_after(state))
                .await;
        }
        if let (Some(execution), Some(session)) = (&execution, &session) {
            ensure!(recovery::valid_session(session), "invalid agent session ID");
            recovery::save_session(&self.paths, &id, execution, session)?;
        }
        let now = i64::try_from(crate::time::unix_seconds())?;
        let report = Report {
            workspace: id,
            state,
            execution,
            at: now,
            prompt_cache_expires_at: expiry(now, ending.prompt_cache)?,
            failed: ending.failed,
        };
        let (status, changed) = self.store.run(move |db| save(db, &report)).await?;
        if changed {
            self.signals.agent_changed();
        }
        Ok(status)
    }

    /// Record the turn `failure` ended as reported from `execution`, which
    /// leaves the agent idle and may retry the turn. The retry is scheduled
    /// first, since the agent going idle finishes a delivered one.
    pub async fn report_turn_failure(
        &self,
        selector: &str,
        failure: TurnFailure,
        execution: Option<String>,
        session: Option<String>,
        turn_end: TurnEnd,
    ) -> Result<AgentStatus> {
        if let Some(execution) = &execution {
            self.retry_failed_turn(selector, failure, execution).await;
        }
        let ending = TurnEnding {
            failed: true,
            prompt_cache: Some(self.prompt_cache(execution.as_deref(), turn_end).await),
            ..TurnEnding::ended()
        };
        self.record_agent_state(selector, AgentState::Idle, execution, session, ending)
            .await
    }
}

/// How a report ends the agent's turn: whether it ends one, whether a
/// failure or an interrupt ended it, and how long its prompt cache lives
/// after it. A report that ends no turn has none of them.
#[derive(Debug, Clone, Copy, Default)]
struct TurnEnding {
    ends: bool,
    failed: bool,
    interrupted: bool,
    prompt_cache: Option<Duration>,
}

impl TurnEnding {
    fn ended() -> Self {
        Self {
            ends: true,
            ..Self::default()
        }
    }

    /// Whether the agent idles after an interrupt once it reports `state`
    /// so: a turn that starts or ends says, while a report that the agent
    /// is still idle, such as Claude Code's notice of a waiting prompt,
    /// leaves it as it was.
    fn interrupted_after(self, state: AgentState) -> Option<bool> {
        (self.ends || state != AgentState::Idle).then_some(self.interrupted)
    }
}

/// A state report as the store records it.
struct Report {
    workspace: String,
    state: AgentState,
    execution: Option<String>,
    /// When it arrived, in Unix seconds.
    at: i64,
    prompt_cache_expires_at: Option<i64>,
    /// Whether a failure ended the agent's turn.
    failed: bool,
}

/// When a prompt cache that lives `lifetime` from `now` expires.
fn expiry(now: i64, lifetime: Option<Duration>) -> Result<Option<i64>> {
    lifetime
        .map(|lifetime| Ok(now.saturating_add(i64::try_from(lifetime.as_secs())?)))
        .transpose()
}

/// Record `report` with the turn retries it settles, in one transaction:
/// the state it leaves and whether it changed it.
fn save(db: &mut Connection, report: &Report) -> Result<(AgentStatus, bool)> {
    let tx = db.transaction()?;
    let previous = get(&tx, &report.workspace)?.map(|status| status.state);
    let changed = write_state(&tx, report)?;
    super::turn_retry::follow_state(&tx, &report.workspace, report.state)?;
    if !report.failed {
        let execution = report.execution.as_deref();
        super::turn_retry::unretried::follow_state(&tx, execution, previous, report.state)?;
    }
    let status = get(&tx, &report.workspace)?.expect("agent state was just recorded");
    tx.commit()?;
    Ok((status, changed))
}

/// Write `report` over the workspace's last one, keeping a repeated state's
/// start: an idle agent's prompt cache expires as a turn just ended says,
/// keeps its expiry while the same execution stays idle, and has none
/// otherwise. Whether anything changed.
fn write_state(db: &Connection, report: &Report) -> Result<bool> {
    let changed = db.execute(
        "INSERT INTO workspace_agent_state
             (workspace_id,state,execution_id,since,prompt_cache_expires_at)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(workspace_id) DO UPDATE SET
             since=CASE WHEN state=excluded.state THEN since ELSE excluded.since END,
             prompt_cache_expires_at=CASE
                 WHEN excluded.state IS NOT 'idle' THEN NULL
                 WHEN excluded.prompt_cache_expires_at IS NOT NULL
                     THEN excluded.prompt_cache_expires_at
                 WHEN state=excluded.state AND execution_id IS excluded.execution_id
                     THEN prompt_cache_expires_at
             END,
             state=excluded.state,execution_id=excluded.execution_id
         WHERE state IS NOT excluded.state
             OR execution_id IS NOT excluded.execution_id
             OR excluded.prompt_cache_expires_at IS NOT NULL",
        params![
            report.workspace,
            report.state,
            report.execution,
            report.at,
            report.prompt_cache_expires_at
        ],
    )?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests;
