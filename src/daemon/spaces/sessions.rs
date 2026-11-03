//! The agent sessions a space's agents reported, which clients find the
//! files of; the daemon never reads them.
use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    client::transcripts::{ReportedSession, SpaceSessions},
    daemon::workspace::Manager,
    execution::recovery::{self, LastSession},
    model::{Chat, PresentSpace, Workspace},
    state::ExecutionState,
};

impl Manager {
    /// The sessions the agents of the workspace or chat `selector` names
    /// reported, with the tools its settings give them and the environments
    /// its repository's agents ran with.
    pub async fn space_sessions(&self, selector: &str) -> Result<SpaceSessions> {
        let space = self.present_space(selector).await?;
        let sessions = self.reported_sessions(&space).await?;
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
            environments: self.agent_environments(space.repository_id()).await?,
        })
    }

    /// The sessions the agents of `space` reported, each once, those running
    /// now first.
    pub(crate) async fn reported_sessions(
        &self,
        space: &PresentSpace,
    ) -> Result<Vec<ReportedSession>> {
        let sessions = match space {
            PresentSpace::Workspace(workspace) => self.workspace_sessions(workspace).await?,
            PresentSpace::Chat(chat) => chat_session(chat).into_iter().collect(),
        };
        let mut seen = BTreeSet::new();
        Ok(sessions
            .into_iter()
            .filter(|reported| seen.insert(reported.session.clone()))
            .collect())
    }

    /// The session each of the workspace's agent executions last reported,
    /// then those of its stopped agents and last sessions.
    async fn workspace_sessions(&self, workspace: &Workspace) -> Result<Vec<ReportedSession>> {
        let paths = &self.paths;
        let mut executions = self.workspace_executions(&workspace.id).await?;
        // The running agents first, as the dedup keeps the first of each.
        executions.sort_by_key(|execution| !runs_agent(execution));
        let running = executions.into_iter().filter_map(|execution| {
            Some(ReportedSession {
                session: recovery::reported_session(paths, &workspace.id, &execution.id)?,
                current: runs_agent(&execution),
                agent: execution.agent?,
                execution: Some(execution.id),
            })
        });
        let stopped = recovery::stopped_agents(paths, &workspace.id)?
            .into_iter()
            .filter_map(|stopped| {
                Some(ReportedSession {
                    agent: stopped.record.agent,
                    session: stopped.record.session?,
                    execution: Some(stopped.execution),
                    current: false,
                })
            });
        let last = LastSession::all(paths, &workspace.id)?
            .into_iter()
            .filter_map(|last| {
                Some(ReportedSession {
                    agent: last.agent,
                    session: last.session?,
                    execution: Some(last.execution),
                    current: false,
                })
            });
        Ok(running.chain(stopped).chain(last).collect())
    }
}

/// Whether `execution`'s wrapper runs its agent now.
fn runs_agent(execution: &crate::model::Execution) -> bool {
    execution.state == ExecutionState::Running && execution.child.is_some()
}

/// The session the chat's hooks reported last, which its agent runs while
/// the chat does.
fn chat_session(chat: &Chat) -> Option<ReportedSession> {
    Some(ReportedSession {
        agent: chat.agent.clone(),
        session: chat.session.clone()?,
        execution: None,
        current: chat.restore.is_none(),
    })
}

#[cfg(test)]
mod tests;
