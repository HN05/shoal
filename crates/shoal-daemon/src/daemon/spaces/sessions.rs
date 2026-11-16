//! The agent sessions a space's agents reported, which clients find the
//! files of; the daemon never reads them.
use crate::execution::recovery::LastSessionExt;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
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
        let sessions = self.reported_sessions(space.id()).await?;
        let settings = self.present_space_settings(&space).await?;
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
            environments: match space.repository_id() {
                Some(repository) => self.agent_environments(repository).await?,
                None => Vec::new(),
            },
        })
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
