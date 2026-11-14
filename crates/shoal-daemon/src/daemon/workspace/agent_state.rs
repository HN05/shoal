//! The turn state agents report from their hooks: a status signal for
//! integrations that never notifies, completes, or changes cleanup.
use crate::client::execution_ids::ExecutionId;
use crate::client::{agent::AgentSessionId, space_ids::SpaceId};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use super::Manager;
use crate::client::space_names::SpaceSelector;
use crate::{
    agents::hook_reports,
    execution::recovery,
    model::{AgentStatus, Execution, PresentSpace},
    process::{
        execution::Processes,
        identity::{AgentTrees, Scan},
    },
    protocol::TurnEnd,
    state::{AgentState, ExecutionState, TurnFailure},
    time::UnixTime,
};

pub(crate) fn get(db: &Connection, id: &SpaceId) -> Result<Option<AgentStatus>> {
    Ok(db
        .query_row(
            "SELECT state,since,prompt_cache_expires_at FROM space_agent_state
             WHERE space_id=?1",
            [id],
            status,
        )
        .optional()?)
}

/// The last reported state while an agent can still be in it: one reported
/// from outside a tracked execution, or from one that runs. An `unknown`
/// execution may have died with its wrapper or may reattach after a daemon
/// restart, so its state stays recorded and shows again once it runs.
pub(crate) fn current(db: &Connection, id: &SpaceId) -> Result<Option<AgentStatus>> {
    Ok(db
        .query_row(
            "SELECT s.state,s.since,s.prompt_cache_expires_at FROM space_agent_state s
             LEFT JOIN executions e ON e.id=s.execution_id
             WHERE s.space_id=?1 AND (s.execution_id IS NULL OR e.state=?2)",
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

/// A space whose agent is idle.
pub(crate) struct Idle {
    pub space: SpaceId,
    /// When it went idle.
    pub since: UnixTime,
    /// The turn generation it is idle in.
    pub turn: i64,
}

/// The spaces whose agent is idle while it can still be, as [`current`]
/// reads it.
pub(crate) fn idle(db: &Connection) -> Result<Vec<Idle>> {
    Ok(db
        .prepare(
            "SELECT s.space_id,s.since,s.turn FROM space_agent_state s
             LEFT JOIN executions e ON e.id=s.execution_id
             WHERE s.state=?1 AND (s.execution_id IS NULL OR e.state=?2)",
        )?
        .query_map(params![AgentState::Idle, ExecutionState::Running], |row| {
            Ok(Idle {
                space: row.get(0)?,
                since: row.get(1)?,
                turn: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether the agent of the space `id` is still idle in the turn generation
/// `turn`, while it can still be, as [`current`] reads it: no turn started
/// since and no other execution reported.
pub(crate) fn idle_in(db: &Connection, id: &SpaceId, turn: i64) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_agent_state s
             LEFT JOIN executions e ON e.id=s.execution_id
             WHERE s.space_id=?1 AND s.state=?2 AND s.turn=?3
                 AND (s.execution_id IS NULL OR e.state=?4))",
        params![id, AgentState::Idle, turn, ExecutionState::Running],
        |row| row.get(0),
    )?)
}

/// Whether an interrupt ended the last turn of the agent `execution` runs,
/// which leaves it idle without the hook that ends its other turns.
pub(crate) fn interrupted(db: &Connection, execution: &ExecutionId) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT interrupted FROM space_agent_state WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// The state the agent `execution` runs reported last, while it lasts.
pub(crate) fn reported_by_execution(
    db: &Connection,
    execution: &ExecutionId,
) -> Result<Option<AgentState>> {
    Ok(db
        .query_row(
            "SELECT state FROM space_agent_state WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?)
}

/// The turn state each execution reported last, and whether an interrupt
/// ended its last turn.
pub(crate) fn reported_turns(db: &Connection) -> Result<HashMap<ExecutionId, (AgentState, bool)>> {
    Ok(db
        .prepare(
            "SELECT execution_id,state,interrupted FROM space_agent_state
             WHERE execution_id IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Every space's last reported state.
pub(crate) fn all(db: &Connection) -> Result<Vec<(SpaceId, AgentState)>> {
    Ok(db
        .prepare("SELECT space_id,state FROM space_agent_state")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The turn a stopped execution last reported, while its record lasts.
struct StoppedTurn {
    workspace: SpaceId,
    state: AgentState,
    execution: Option<Execution>,
}

impl StoppedTurn {
    /// The turn to keep: an idle one counts as working while `scan` finds
    /// work of the execution outside its process group, short of what other
    /// executions' `agents` run, as protection counts it, since that work
    /// may start its next turn.
    fn kept(&self, scan: Option<(&Scan, &AgentTrees)>) -> AgentState {
        let in_flight = || {
            self.execution
                .as_ref()
                .zip(scan)
                .is_some_and(|(execution, (scan, agents))| {
                    Processes::runs_outside_group(execution, scan, agents)
                })
        };
        if self.state == AgentState::Idle && in_flight() {
            AgentState::Working
        } else {
            self.state
        }
    }
}

fn reported_by(db: &Connection, id: &ExecutionId) -> Result<Option<StoppedTurn>> {
    let reported: Option<(SpaceId, AgentState)> = db
        .query_row(
            "SELECT space_id,state FROM space_agent_state WHERE execution_id=?1",
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
    /// The turn state the agent of `id` reported last, which a stop's
    /// handoff keeps; unknown when it cannot be read.
    pub(crate) async fn reported_turn(&self, id: &ExecutionId) -> Option<AgentState> {
        let execution = id.to_owned();
        let reported = (self.store)
            .run(move |db| reported_by_execution(db, &execution))
            .await;
        reported.unwrap_or_else(|error| {
            crate::daemon::log!("turn of execution {id} unreadable: {error:#}");
            None
        })
    }

    /// Keep the state execution `id` last reported in the recovery record it
    /// saved, before the state goes with the execution's record, judged by
    /// `scan` when its processes were scanned. A failure leaves the turn
    /// unknown, as in records saved before turns were kept.
    pub(crate) async fn keep_stopped_turn(&self, id: &ExecutionId, scan: Option<&Scan>) {
        let query = id.to_owned();
        let kept = async {
            let reported = self.store.run(move |db| reported_by(db, &query)).await?;
            let Some(turn) = reported else {
                return Ok(());
            };
            let agents = match scan {
                Some(_) => Some(self.agent_trees().await?),
                None => None,
            };
            let scan = scan.zip(agents.as_ref());
            recovery::save_turn(&self.paths, &turn.workspace, id, turn.kept(scan))
        }
        .await;
        if let Err(error) = kept {
            crate::daemon::log!("turn state of stopped execution {id} not kept: {error:#}");
        }
    }

    /// Record the state reported from `execution`, which clears it when its
    /// record goes, and the agent `session` it reports for resuming it, as
    /// its hook `made` the report, from when it counts, as one that arrives
    /// late does. A report with `turn_end` ends a turn, as
    /// [`Self::end_turn`] records it. Repeating the current report writes
    /// nothing, since hooks repeat it after every tool call, and a repeated
    /// state keeps its start.
    pub(crate) async fn report_agent_state(
        &self,
        selector: &SpaceSelector,
        state: AgentState,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        turn_end: Option<TurnEnd>,
        made: Option<SystemTime>,
    ) -> Result<AgentStatus> {
        let _order = self.report_order(selector).await?;
        if !self.newest_report(selector, made).await? {
            return self.recorded_status(selector, state).await;
        }
        match turn_end {
            Some(turn_end) => {
                self.end_turn(selector, execution, session, turn_end, made)
                    .await
            }
            None => {
                let ending = TurnEnding::default();
                self.record_agent_state(selector, state, execution, session, ending, made)
                    .await
            }
        }
    }

    /// Whether a report its hook `made` for the space `selector` names is
    /// newer than every report recorded for it, as one without a time is:
    /// a report made before one recorded already, which arrives late, is
    /// left out with all it would change.
    async fn newest_report(
        &self,
        selector: &SpaceSelector,
        made: Option<SystemTime>,
    ) -> Result<bool> {
        let Some(made) = made else {
            return Ok(true);
        };
        let space = self.space(selector).await?;
        // A report a hook still sends, made later, may be recorded already
        // without a mark yet, as across a daemon restart.
        let sending = hook_reports::sending(&self.paths, space.id())?;
        if sending.is_some_and(|sending| sending > made) {
            return Ok(false);
        }
        self.latest_report(space.id(), made).await
    }

    /// The state recorded for the space `selector` names, or `state`, as a
    /// report leaves it that is not recorded.
    async fn recorded_status(
        &self,
        selector: &SpaceSelector,
        state: AgentState,
    ) -> Result<AgentStatus> {
        let id = self.space(selector).await?.id().to_owned();
        let recorded = self.store.run(move |db| get(db, &id)).await?;
        Ok(recorded.unwrap_or_else(|| unrecorded(state, UnixTime::now())))
    }

    /// Hold the order of the state reports of the space `selector` names
    /// until the guard drops, so that a report that arrives late and is
    /// found the newest stays so while it and what it changes are recorded.
    pub(super) async fn report_order(
        &self,
        selector: &SpaceSelector,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let space = self.space(selector).await?;
        Ok(self.report_times.order(space.id()).await)
    }

    /// Record that the agent reporting from `execution` ended a turn and is
    /// idle, its prompt cache expiring the lifetime of its AI tool from now,
    /// and ask the user the question the turn asked in its chat, for a
    /// report made at `made`, as [`Self::record_agent_state`] takes it,
    /// while the caller holds the space's [`Self::report_order`].
    pub(super) async fn end_turn(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        turn_end: TurnEnd,
        made: Option<SystemTime>,
    ) -> Result<AgentStatus> {
        let ending = TurnEnding {
            prompt_cache: Some(
                self.prompt_cache(selector, execution.as_ref(), &turn_end)
                    .await,
            ),
            ..TurnEnding::ended()
        };
        self.ask_marked_chat_question(selector, execution.as_ref(), turn_end.final_message)
            .await;
        self.record_agent_state(selector, AgentState::Idle, execution, session, ending, made)
            .await
    }

    /// Record the turn a wake hook starts for the agent `execution` runs,
    /// or a chat's agent without one, unless a stop of the agent came first:
    /// the hook then keeps the update for the agent's restore. Whether the
    /// turn starts.
    pub(crate) async fn start_woken_turn(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
    ) -> Result<bool> {
        let _order = self.report_order(selector).await?;
        // A protection stop is decided against the turn in the transaction
        // that would record it; others show only on the agent's connection.
        if let Some(id) = &execution
            && self.agent_stopping(id).await
        {
            return Ok(false);
        }
        let recorded = self
            .take_agent_state(
                selector,
                AgentState::Working,
                execution.clone(),
                None,
                TurnEnding {
                    woken: true,
                    ..TurnEnding::default()
                },
                None,
            )
            .await?;
        // A stopped agent, or a chat whose save came first, in the
        // transaction that would have recorded the turn, takes no report:
        // the update that woke its hook, such as the one a blocked agent's
        // stop leaves, waits for its restore.
        Ok(recorded.is_some())
    }

    /// Record that an interrupt ended the turn of the agent reporting from
    /// `execution`, which leaves it idle without the hook that ends its
    /// other turns.
    pub(crate) async fn end_interrupted_turn(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
    ) -> Result<AgentStatus> {
        let _order = self.report_order(selector).await?;
        let ending = TurnEnding {
            interrupted: true,
            prompt_cache: Some(
                self.prompt_cache(selector, execution.as_ref(), &TurnEnd::default())
                    .await,
            ),
            ..TurnEnding::ended()
        };
        self.record_agent_state(selector, AgentState::Idle, execution, None, ending, None)
            .await
    }

    /// How long the prompt cache of the agent in the space `selector` names
    /// lives after the turn `turn_end` reports: as its AI tool resolves
    /// there, or as reported for an agent without one, else the default
    /// lifetime.
    async fn prompt_cache(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        turn_end: &TurnEnd,
    ) -> Duration {
        let reported = turn_end.prompt_cache_secs.map(Duration::from_secs);
        match self.agent_tool(selector, execution).await {
            Some(tool) => tool.prompt_cache(reported),
            None => reported.unwrap_or(crate::ai::DEFAULT_PROMPT_CACHE),
        }
    }

    /// The AI tool of the agent in the space `selector` names, as the
    /// space's settings resolve it: a chat's own agent, or the one
    /// `execution` runs in a workspace.
    async fn agent_tool(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
    ) -> Option<crate::ai::Tool> {
        let space = self.space(selector).await.ok()?;
        let agent = match (&space, execution) {
            (PresentSpace::Chat(chat), _) => chat.agent.clone(),
            (PresentSpace::Workspace(_), Some(execution)) => self.agent_name(execution).await?,
            (PresentSpace::Workspace(_), None) => return None,
        };
        let mut tools = self.present_space_settings(&space).await.ok()?.tools;
        tools.remove(agent.name())
    }

    /// Record `state` as [`Self::report_agent_state`] does, for a report that
    /// `ending` says how it ends the agent's turn, while the caller holds
    /// the space's [`Self::report_order`]. A report that arrives late, from
    /// a hook that could not send it, counts from when it was `made`.
    pub(super) async fn record_agent_state(
        &self,
        selector: &SpaceSelector,
        state: AgentState,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        ending: TurnEnding,
        made: Option<SystemTime>,
    ) -> Result<AgentStatus> {
        let taken =
            (self.take_agent_state(selector, state, execution, session, ending, made)).await?;
        Ok(taken.unwrap_or_else(|| unrecorded(state, UnixTime::now())))
    }

    /// [`Self::record_agent_state`], returning the status the space takes,
    /// or none when it takes no report, as a chat that stopped.
    async fn take_agent_state(
        &self,
        selector: &SpaceSelector,
        state: AgentState,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        ending: TurnEnding,
        made: Option<SystemTime>,
    ) -> Result<Option<AgentStatus>> {
        let space = self.space(selector).await?;
        if let Some(session) = session {
            self.record_agent_session(&space, execution.as_ref(), session)
                .await?;
        }
        let made = made.unwrap_or_else(SystemTime::now);
        // A late turn's prompt cache, too, lives from when the turn ended.
        let at = UnixTime::from_system_time(made);
        let report = Report {
            space: space.id().to_owned(),
            state,
            execution,
            at,
            prompt_cache_expires_at: ending.prompt_cache.map(|lifetime| at + lifetime),
            failed: ending.failed,
            interrupted: ending.interrupted_after(state),
            woken: ending.woken,
        };
        let saved = self.store.write(move |tx| save(tx, &report)).await?;
        self.report_times.arrived(space.id(), made);
        let Some((status, changed)) = saved else {
            return Ok(None);
        };
        if changed {
            self.signals.agent_changed();
        }
        Ok(Some(status))
    }

    /// Record the agent `session` reported in `space`: the session of the
    /// agent `execution` runs, for resuming it, and a chat's own, which its
    /// export continues elsewhere.
    async fn record_agent_session(
        &self,
        space: &PresentSpace,
        execution: Option<&ExecutionId>,
        session: AgentSessionId,
    ) -> Result<()> {
        ensure!(
            recovery::valid_session(session.as_str()),
            "invalid agent session ID"
        );
        match (space, execution) {
            (_, Some(execution)) => {
                // A subscription follows the session, which the store leaves out.
                if recovery::save_session(&self.paths, space.id(), execution, &session)? {
                    self.signals.agent_changed();
                }
            }
            // The agent of a chat that became the workspace, which its
            // hand-over continues.
            (PresentSpace::Workspace(workspace), None) => {
                self.record_converting_session(&workspace.id, session.clone())
                    .await?
            }
            (PresentSpace::Chat(_), None) => {}
        }
        if let PresentSpace::Chat(chat) = space {
            self.record_chat_session(&chat.id, session).await?;
        }
        Ok(())
    }

    /// Record the turn `failure` ended as reported from `execution`, as its
    /// hook `made` the report, which leaves the agent idle and may retry
    /// the turn. The retry is scheduled
    /// first, since the agent going idle finishes a delivered one.
    pub async fn report_turn_failure(
        &self,
        selector: &SpaceSelector,
        failure: TurnFailure,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        turn_end: TurnEnd,
        made: Option<SystemTime>,
    ) -> Result<AgentStatus> {
        let _order = self.report_order(selector).await?;
        if !self.newest_report(selector, made).await? {
            return self.recorded_status(selector, AgentState::Idle).await;
        }
        self.fail_turn(selector, failure, execution, session, turn_end, made)
            .await
    }

    /// [`Self::report_turn_failure`] for a report made at `made`, as
    /// [`Self::record_agent_state`] takes it, while the caller holds the
    /// space's [`Self::report_order`].
    pub(super) async fn fail_turn(
        &self,
        selector: &SpaceSelector,
        failure: TurnFailure,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        turn_end: TurnEnd,
        made: Option<SystemTime>,
    ) -> Result<AgentStatus> {
        if let Some(execution) = &execution {
            self.retry_failed_turn(selector, failure, execution).await;
        }
        let ending = TurnEnding {
            failed: true,
            prompt_cache: Some(
                self.prompt_cache(selector, execution.as_ref(), &turn_end)
                    .await,
            ),
            ..TurnEnding::ended()
        };
        self.record_agent_state(selector, AgentState::Idle, execution, session, ending, made)
            .await
    }
}

/// How a report ends the agent's turn: whether it ends one, whether a
/// failure or an interrupt ended it, and how long its prompt cache lives
/// after it, or whether a wake hook starts the turn it reports. A report
/// that ends no turn has none of the former.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TurnEnding {
    woken: bool,
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

/// The status of a report for a space that takes none, such as a chat
/// whose agent stopped before the report arrived, which is not recorded.
fn unrecorded(state: AgentState, now: UnixTime) -> AgentStatus {
    AgentStatus {
        state,
        since: now,
        prompt_cache_expires_at: None,
    }
}

/// A state report as the store records it.
struct Report {
    space: SpaceId,
    state: AgentState,
    execution: Option<ExecutionId>,
    /// When it was made, which is when it arrived unless a hook could not
    /// send it.
    at: UnixTime,
    prompt_cache_expires_at: Option<UnixTime>,
    /// Whether a failure ended the agent's turn.
    failed: bool,
    /// Whether an interrupt ended it, when the report says.
    interrupted: Option<bool>,
    /// Whether a wake hook starts the turn it reports.
    woken: bool,
}

impl Report {
    /// Whether the report starts a turn after the space's `previous` one:
    /// a woken turn, a working agent that was idle or waiting, or the first
    /// report of another execution, which a decision made for the previous
    /// one must tell apart too.
    fn starts_turn(&self, previous: Option<&Recorded>) -> bool {
        let Some(previous) = previous else {
            return true;
        };
        self.woken
            || previous.execution != self.execution
            || (self.state == AgentState::Working && previous.state != AgentState::Working)
    }
}

/// The state a space recorded last, and the execution that reported it.
struct Recorded {
    state: AgentState,
    execution: Option<ExecutionId>,
}

fn recorded(db: &Connection, id: &SpaceId) -> Result<Option<Recorded>> {
    Ok(db
        .query_row(
            "SELECT state,execution_id FROM space_agent_state WHERE space_id=?1",
            [id],
            |row| {
                Ok(Recorded {
                    state: row.get(0)?,
                    execution: row.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Record `report` with the turn retries it settles and the sleep a turn it
/// starts ends, in one transaction: the state it leaves and whether it
/// changed it, or `None` when its space takes no report by then, or its
/// agent no turn since Shoal decided to stop it.
fn save(tx: &rusqlite::Transaction<'_>, report: &Report) -> Result<Option<(AgentStatus, bool)>> {
    if !takes_report(tx, &report.space)? {
        return Ok(None);
    }
    let recorded = recorded(tx, &report.space)?;
    let starts = report.starts_turn(recorded.as_ref());
    if starts && report.state == AgentState::Working && stopped(tx, report)? {
        return Ok(None);
    }
    let changed = write_state(tx, report, starts)?;
    let previous = recorded.map(|recorded| recorded.state);
    super::turn_retry::follow_state(tx, &report.space, report.state)?;
    crate::daemon::idle_wakes::sleep::follow_state(tx, &report.space, previous, report.state)?;
    if !report.failed {
        let execution = report.execution.as_ref();
        super::turn_retry::unretried::follow_state(tx, execution, previous, report.state)?;
    }
    let status = get(tx, &report.space)?.expect("agent state was just recorded");
    Ok(Some((status, changed)))
}

/// Whether Shoal decided to stop the agent `report` comes from, whose turn
/// then waits for its restore.
fn stopped(db: &Connection, report: &Report) -> Result<bool> {
    match &report.execution {
        Some(execution) => super::agents::stop_recorded(db, execution),
        None => Ok(false),
    }
}

/// Whether the space `id` takes a state report: a workspace, or a chat
/// whose agent runs. A chat's stop or removal clears its state, so a report
/// checked against the chat before either would otherwise outlive it.
fn takes_report(db: &Connection, id: &SpaceId) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS (SELECT 1 FROM workspaces WHERE id=?1)
            OR EXISTS (SELECT 1 FROM chats WHERE id=?1
                AND EXISTS (SELECT 1 FROM executions WHERE space_id=?1))",
        [id],
        |row| row.get(0),
    )?)
}

/// Write `report` over the space's last one, keeping a repeated state's
/// start: an idle agent's prompt cache expires as a turn just ended says,
/// keeps its expiry while the same execution stays idle, and has none
/// otherwise. A report that `starts` a turn counts it in the space's turn
/// generation, which a new state continues from the space's last count. An
/// interrupt stays with the execution whose turn it ended.
/// Whether anything changed.
fn write_state(db: &Connection, report: &Report, starts: bool) -> Result<bool> {
    let changed = db.execute(
        "INSERT INTO space_agent_state
             (space_id,state,execution_id,since,prompt_cache_expires_at,turn,interrupted)
         VALUES (?1,?2,?3,?4,?5,
             COALESCE((SELECT turn FROM space_turns WHERE space_id=?1),0)+1,
             COALESCE(?7,0))
         ON CONFLICT(space_id) DO UPDATE SET
             since=CASE WHEN state=excluded.state THEN since ELSE excluded.since END,
             prompt_cache_expires_at=CASE
                 WHEN excluded.state IS NOT 'idle' THEN NULL
                 WHEN excluded.prompt_cache_expires_at IS NOT NULL
                     THEN excluded.prompt_cache_expires_at
                 WHEN state=excluded.state AND execution_id IS excluded.execution_id
                     THEN prompt_cache_expires_at
             END,
             turn=turn+?6,
             interrupted=CASE WHEN execution_id IS excluded.execution_id
                 THEN COALESCE(?7,interrupted) ELSE COALESCE(?7,0) END,
             state=excluded.state,execution_id=excluded.execution_id
         WHERE state IS NOT excluded.state
             OR execution_id IS NOT excluded.execution_id
             OR excluded.prompt_cache_expires_at IS NOT NULL
             OR ?6
             OR interrupted IS NOT COALESCE(?7,interrupted)",
        params![
            report.space,
            report.state,
            report.execution,
            report.at,
            report.prompt_cache_expires_at,
            starts,
            report.interrupted,
        ],
    )?;
    Ok(changed > 0)
}

/// The reports tests make, as a hook's request makes them when it arrives.
#[cfg(test)]
impl Manager {
    pub async fn set_agent_state(
        &self,
        selector: &SpaceSelector,
        state: AgentState,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
    ) -> Result<AgentStatus> {
        self.report_agent_state(selector, state, execution, session, None, None)
            .await
    }

    pub async fn end_agent_turn(
        &self,
        selector: &SpaceSelector,
        execution: Option<ExecutionId>,
        session: Option<AgentSessionId>,
        turn_end: TurnEnd,
    ) -> Result<AgentStatus> {
        let idle = AgentState::Idle;
        self.report_agent_state(selector, idle, execution, session, Some(turn_end), None)
            .await
    }
}

#[cfg(test)]
mod tests;
