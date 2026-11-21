//! A space's agent and the sessions its agents reported, which clients find
//! the files of; the daemon never reads them. Its executions and their
//! saved sessions record them for every space, whatever its kind.
use crate::execution::recovery::LastSessionExt;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use crate::{
    agent::Agent,
    daemon::manager::Manager,
    execution::recovery::{self, LastSession},
    paths::Paths,
    state::ExecutionState,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::{ReportedSession, SpaceSessions};

impl Manager {
    /// The sessions the agents of the workspace or chat `selector` names
    /// reported, with the tools its settings give them and the environments
    /// its repository's agents ran with.
    pub async fn space_sessions(&self, selector: &SpaceSelector) -> Result<SpaceSessions> {
        let space = self.space(selector).await?;
        let sessions = self.reported_sessions(&space.id).await?;
        let settings = self.repositories.space_settings(self, &space).await?;
        let integrations = sessions
            .iter()
            .map(|reported| {
                (
                    reported.agent.clone(),
                    settings.integration(&reported.agent),
                )
            })
            .collect();
        Ok(SpaceSessions {
            sessions,
            integrations,
            environments: match space.repository_id.as_ref() {
                Some(repository) => self.agent_environments(repository).await?,
                None => Vec::new(),
            },
        })
    }

    /// The agent of the space `id`, named as `--agent` names it: the one
    /// its execution runs, else the one it saved last, stopped or as a last
    /// session, else the one its queued launch starts; none for a space
    /// whose saved sessions were discarded and that waits for no launch.
    pub(crate) async fn agent_of(&self, id: &SpaceId) -> Result<Option<Agent>> {
        let space = id.to_owned();
        let stored = (self.store)
            .read(move |tx| stored_agents(tx, &space))
            .await?;
        self.agent_from(id, stored)
    }

    /// [`Self::agent_of`] the space `id`, of whose agent the store records
    /// `stored`; only a space whose execution runs no agent reads what it
    /// saved.
    pub(crate) fn agent_from(&self, id: &SpaceId, stored: StoredAgents) -> Result<Option<Agent>> {
        let agent = match stored.running {
            Some(running) => Some(running),
            None => saved_agent(&self.paths, id)?.or(stored.queued),
        };
        Ok(agent.map(|agent| agent.normalized()))
    }

    /// The sessions the agents of the space `id` reported, each once, those
    /// running now first: the session each of its agent executions last
    /// reported, then those of its stopped agents and last sessions.
    pub(crate) async fn reported_sessions(&self, id: &SpaceId) -> Result<Vec<ReportedSession>> {
        let mut sessions = self.execution_sessions(id).await?;
        sessions.extend(stopped_sessions(&self.paths, id)?);
        sessions.extend(last_sessions(&self.paths, id)?);
        let mut seen = BTreeSet::new();
        Ok(sessions
            .into_iter()
            .filter(|reported| seen.insert(reported.session.clone()))
            .collect())
    }

    /// The session each of the agent executions of the space `id` last
    /// reported, those that run their agent first.
    async fn execution_sessions(&self, id: &SpaceId) -> Result<Vec<ReportedSession>> {
        let mut executions = self.workspace_executions(id).await?;
        executions.sort_by_key(|execution| !runs_agent(execution));
        Ok(executions
            .into_iter()
            .filter_map(|execution| {
                Some(ReportedSession {
                    session: recovery::reported_session(&self.paths, id, &execution.id)?,
                    current: runs_agent(&execution),
                    agent: execution.agent?,
                    execution: Some(execution.id),
                })
            })
            .collect())
    }
}

/// What the store records of a space's agent: the one its execution runs
/// and the one its queued launch starts.
pub(crate) struct StoredAgents {
    running: Option<Agent>,
    queued: Option<Agent>,
}

/// What the store records of the agent of the space `id`.
pub(crate) fn stored_agents(db: &Connection, id: &SpaceId) -> Result<StoredAgents> {
    Ok(StoredAgents {
        running: running_agent(db, id)?,
        queued: queued_agent(db, id)?,
    })
}

/// The agent an execution of the space `id` runs, the newest first.
fn running_agent(db: &Connection, id: &SpaceId) -> Result<Option<Agent>> {
    Ok(db
        .query_row(
            "SELECT agent FROM executions WHERE space_id=?1 AND agent IS NOT NULL
             ORDER BY rowid DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The agent the newest queued launch of the space `id` starts.
fn queued_agent(db: &Connection, id: &SpaceId) -> Result<Option<Agent>> {
    Ok(db
        .query_row(
            "SELECT agent FROM agent_queue WHERE space_id=?1 ORDER BY rowid DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// The agent the space `space_id` saved last: its newest stopped agent's,
/// else its newest last session's.
fn saved_agent(paths: &Paths, space_id: &SpaceId) -> Result<Option<Agent>> {
    let stopped = recovery::stopped_agents(paths, space_id)?;
    let newest = (stopped.into_iter()).max_by_key(|stopped| stopped.stopped_at);
    if let Some(stopped) = newest {
        return Ok(Some(stopped.record.agent));
    }
    let last = LastSession::all(paths, space_id)?;
    let newest = (last.into_iter()).max_by_key(|last| last.stop.as_ref().and_then(|stop| stop.at));
    Ok(newest.map(|last| last.agent))
}

/// The sessions of the space's stopped agents.
fn stopped_sessions(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ReportedSession>> {
    Ok(recovery::stopped_agents(paths, space_id)?
        .into_iter()
        .filter_map(|stopped| {
            Some(ReportedSession {
                agent: stopped.record.agent,
                session: stopped.record.session?,
                execution: Some(stopped.execution),
                current: false,
            })
        })
        .collect())
}

/// The sessions of the agents that exited, the space's last sessions.
fn last_sessions(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ReportedSession>> {
    Ok(LastSession::all(paths, space_id)?
        .into_iter()
        .filter_map(|last| {
            Some(ReportedSession {
                agent: last.agent,
                session: last.session?,
                execution: Some(last.execution),
                current: false,
            })
        })
        .collect())
}

/// Whether `execution`'s wrapper runs its agent now.
fn runs_agent(execution: &crate::model::Execution) -> bool {
    execution.state == ExecutionState::Running && execution.child.is_some()
}

#[cfg(test)]
mod tests;
