//! The subagents each tracked agent runs, as its hooks report their starts
//! and stops: an idle agent with one running counts as working, since the
//! subagent works for it and its end may start the agent's next turn. A
//! subagent whose stop never arrives counts until the agent's session
//! starts again or its execution ends, or, once the proxy of its execution
//! has passed a request of its subagents in the current session, until none
//! has for [`LOST_AFTER`].
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;
use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Result, ensure};
use rusqlite::{Connection, params};

use crate::{Agents, daemon::Daemon, protocol::SubagentChange, time::UnixTime};

/// How long a subagent counts after its start and the last request of its
/// agent's subagents through the proxy: generous, since a subagent may run
/// a long tool call without one.
pub const LOST_AFTER: Duration = Duration::from_secs(60 * 60);

/// The longest subagent ID a hook reports.
const MAX_AGENT_ID_LEN: usize = 256;

impl Agents {
    /// Record `change` to the subagents of the agent `execution` runs in
    /// the space `selector` names, at `now`; a report from no execution
    /// counts none, since only an execution's agent is put to sleep.
    pub async fn report_subagent(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
        change: SubagentChange,
        now: UnixTime,
    ) -> Result<()> {
        daemon.space(selector).await?;
        let Some(execution) = execution else {
            return Ok(());
        };
        check(&change)?;
        (self.store)
            .write(move |tx| record(tx, &execution, &change, now))
            .await
    }

    /// Record that a request of the subagents of the agent `execution` runs
    /// in the space `selector` names passed its proxy at `at`.
    pub async fn report_subagent_request(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: ExecutionId,
        at: UnixTime,
    ) -> Result<()> {
        let space = daemon.space(selector).await?.id;
        (self.store)
            .write(move |tx| record_request(tx, &space, &execution, at))
            .await
    }
}

/// Refuse `change` when it names a subagent by an ID too long to record.
fn check(change: &SubagentChange) -> Result<()> {
    if let SubagentChange::Started { agent_id } | SubagentChange::Stopped { agent_id } = change {
        ensure!(agent_id.len() <= MAX_AGENT_ID_LEN, "subagent ID too long");
    }
    Ok(())
}

/// Record `change` to the subagents of `execution` at `now`: a start or a
/// stop counts one more, and a session start stops every one that runs and
/// forgets the requests the proxy passed before it, since a restore that
/// starts it may run the agent without that proxy.
fn record(
    db: &Connection,
    execution: &ExecutionId,
    change: &SubagentChange,
    now: UnixTime,
) -> Result<()> {
    match change {
        SubagentChange::Started { agent_id } => db.execute(
            "INSERT INTO execution_subagents(execution_id,agent_id,started_at,starts)
             SELECT id,?2,?3,1 FROM executions WHERE id=?1
             ON CONFLICT(execution_id,agent_id) DO UPDATE SET
                 starts=starts+1,started_at=MAX(started_at,excluded.started_at)",
            params![execution, agent_id, now],
        )?,
        SubagentChange::Stopped { agent_id } => db.execute(
            "INSERT INTO execution_subagents(execution_id,agent_id,started_at,stops)
             SELECT id,?2,?3,1 FROM executions WHERE id=?1
             ON CONFLICT(execution_id,agent_id) DO UPDATE SET stops=stops+1",
            params![execution, agent_id, now],
        )?,
        SubagentChange::SessionStarted => {
            db.execute(
                "UPDATE execution_subagents SET stops=starts
                 WHERE execution_id=?1 AND starts>stops",
                [execution],
            )?;
            db.execute(
                "DELETE FROM execution_subagent_requests WHERE execution_id=?1",
                [execution],
            )?
        }
    };
    Ok(())
}

/// Record that a request of the subagents of `execution`, an execution of
/// the space `space`, passed its proxy at `at`, unless a later one did.
fn record_request(
    db: &Connection,
    space: &SpaceId,
    execution: &ExecutionId,
    at: UnixTime,
) -> Result<()> {
    db.execute(
        "INSERT INTO execution_subagent_requests(execution_id,at)
         SELECT id,?2 FROM executions WHERE id=?1 AND space_id=?3
         ON CONFLICT(execution_id) DO UPDATE SET at=MAX(at,excluded.at)",
        params![execution, at, space],
    )?;
    Ok(())
}

/// The subagents running at `?1`, a time [`LOST_AFTER`] before then:
/// started more often than stopped, and unless the proxy has passed none of
/// their requests, started or with a request since.
const RUNNING: &str = "execution_subagents s
     LEFT JOIN execution_subagent_requests r ON r.execution_id=s.execution_id
     WHERE s.starts>s.stops AND (r.at IS NULL OR MAX(s.started_at,r.at)>?1)";

/// The executions whose agent runs a subagent at `now`.
pub fn running(db: &Connection, now: UnixTime) -> Result<HashSet<ExecutionId>> {
    Ok(db
        .prepare(&format!("SELECT DISTINCT s.execution_id FROM {RUNNING}"))?
        .query_map([now - LOST_AFTER], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether the agent of execution `id` runs a subagent at `now`.
pub fn runs_one(db: &Connection, id: &ExecutionId, now: UnixTime) -> Result<bool> {
    Ok(db.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM {RUNNING} AND s.execution_id=?2)"),
        params![now - LOST_AFTER, id],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests;
