//! A space's agent and the sessions its agents reported, whose files
//! clients find. The daemon never reads them.
use crate::Spaces;
use crate::daemon::{Daemon, Records};
use crate::execution::recovery::LastSessionExt;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::Connection;

use crate::{
    agent::Agent,
    conversions::handoff::Leaving,
    execution::recovery::{self, LastSession},
    paths::Paths,
    state::ExecutionState,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::{ReportedSession, SpaceSessions};

impl Spaces {
    /// With the tools the space's settings give them and the environments
    /// its repository's agents ran with.
    pub async fn space_sessions(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SpaceSessions> {
        let space = self.space(selector).await?;
        let sessions = self.reported_sessions(&space.id).await?;
        let settings = daemon.space_settings(&space).await?;
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
                Some(repository) => daemon.agent_environments(repository).await?,
                None => Vec::new(),
            },
        })
    }

    /// Named as `--agent` names it: the one its execution runs, else the one
    /// it saved last, stopped or as a last session, else the one its queued
    /// launch starts. None for a space whose saved sessions were discarded
    /// and that waits for no launch.
    pub async fn agent_of(&self, id: &SpaceId) -> Result<Option<Agent>> {
        let (space, records) = (id.to_owned(), self.records.clone());
        let stored = (self.store)
            .read(move |tx| stored_agents(tx, &*records, &space))
            .await?;
        self.agent_from(id, stored)
    }

    /// [`Self::agent_of`] from what the store records. Only a space whose
    /// execution runs no agent reads what it saved.
    pub fn agent_from(&self, id: &SpaceId, stored: StoredAgents) -> Result<Option<Agent>> {
        let agent = match stored.running {
            Some(running) => Some(running),
            None => saved_agent(&self.paths, id)?.or(stored.queued),
        };
        Ok(agent.map(|agent| agent.normalized()))
    }

    /// Each once, those running now first: the session each agent execution
    /// last reported, then those of its stopped agents and last sessions,
    /// each with the session a handoff made it continue from.
    pub async fn reported_sessions(&self, id: &SpaceId) -> Result<Vec<ReportedSession>> {
        let mut sessions = self.execution_sessions(id).await?;
        sessions.extend(stopped_sessions(&self.paths, id)?);
        sessions.extend(last_sessions(&self.paths, id)?);
        let space = id.to_owned();
        let mut handed_over = (self.store)
            .read(move |db| crate::store::agent_handoffs::continued_from(db, &space))
            .await?;
        let mut seen = BTreeSet::new();
        Ok(sessions
            .into_iter()
            .filter(|reported| seen.insert(reported.session.clone()))
            .map(|reported| ReportedSession {
                continued_from: handed_over.remove(&reported.session),
                ..reported
            })
            .collect())
    }

    /// Those that run their agent first.
    async fn execution_sessions(&self, id: &SpaceId) -> Result<Vec<ReportedSession>> {
        let mut executions = self.workspace_executions(id).await?;
        executions.sort_by_key(|execution| !runs_agent(execution));
        Ok(executions
            .into_iter()
            .filter_map(|execution| {
                Some(ReportedSession {
                    session: recovery::carried::continued(&self.paths, id, &execution.id).session?,
                    current: runs_agent(&execution),
                    agent: execution.agent?,
                    execution: Some(execution.id),
                    continued_from: None,
                })
            })
            .collect())
    }
}

/// The one its execution runs and the one its queued launch starts.
pub struct StoredAgents {
    running: Option<Agent>,
    queued: Option<Agent>,
}

pub fn stored_agents(db: &Connection, records: &dyn Records, id: &SpaceId) -> Result<StoredAgents> {
    Ok(StoredAgents {
        running: crate::executions::newest_agent(db, id)?,
        queued: records.queued_agent(db, id)?,
    })
}

fn saved_agent(paths: &Paths, space_id: &SpaceId) -> Result<Option<Agent>> {
    Ok(newest_saved(paths, space_id)?.map(|saved| saved.agent))
}

/// With its session, the execution it ran as and where it ran: the newest
/// stopped agent, else the newest last session.
pub fn newest_saved(paths: &Paths, space_id: &SpaceId) -> Result<Option<Leaving>> {
    let stopped = recovery::stopped_agents(paths, space_id)?;
    let newest = (stopped.into_iter()).max_by_key(|stopped| stopped.stopped_at);
    if let Some(stopped) = newest {
        return Ok(Some(Leaving {
            agent: stopped.record.agent,
            session: stopped.record.session,
            execution: stopped.execution,
            origin: stopped.record.origin,
        }));
    }
    let last = LastSession::all(paths, space_id)?;
    let newest = (last.into_iter()).max_by_key(|last| last.stop.as_ref().and_then(|stop| stop.at));
    Ok(newest.map(|last| Leaving {
        agent: last.agent,
        session: last.session,
        execution: last.execution,
        origin: last.origin,
    }))
}

fn stopped_sessions(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ReportedSession>> {
    Ok(recovery::stopped_agents(paths, space_id)?
        .into_iter()
        .filter_map(|stopped| {
            Some(ReportedSession {
                agent: stopped.record.agent,
                session: stopped.record.session?,
                execution: Some(stopped.execution),
                current: false,
                continued_from: None,
            })
        })
        .collect())
}

/// The agents that exited are the space's last sessions.
fn last_sessions(paths: &Paths, space_id: &SpaceId) -> Result<Vec<ReportedSession>> {
    Ok(LastSession::all(paths, space_id)?
        .into_iter()
        .filter_map(|last| {
            Some(ReportedSession {
                agent: last.agent,
                session: last.session?,
                execution: Some(last.execution),
                current: false,
                continued_from: None,
            })
        })
        .collect())
}

fn runs_agent(execution: &crate::model::Execution) -> bool {
    execution.state == ExecutionState::Running && execution.child.is_some()
}
