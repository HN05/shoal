//! Handing a space's work from one agent tool to another: copies of the
//! sessions its agents reported go into its attached files, and the new
//! tool starts a new session told where they are. Shoal never converts one
//! tool's session into another's format, and a tool the space used before
//! starts a new session too.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::{
    agent_session_ids::AgentSessionId, execution_ids::ExecutionId, space_ids::SpaceId,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{Agent, ModelChoice},
    config::{modes::ModeName, permissions::PermissionsConfig},
    execution::recovery::{HandoffBrief, LastSession, LastSessionExt as _},
    sessions::Origin,
    stops::StopRequest,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::HandedOverSession;

mod brief;
mod copies;

/// The agent a handoff takes a space's work from: its tool, the session it
/// reported last, the execution it ran as and where it ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leaving {
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AgentSessionId>,
    pub execution: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// The agent that takes a space's work over: its tool and mode, the model
/// asked of it, the permissions given to it and the note it is told after
/// its mode's instructions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Taking {
    pub agent: Agent,
    pub mode: ModeName,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default)]
    pub narrowing: PermissionsConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Spaces {
    /// Hand the work of the space `id` over from `leaving`, the agent it
    /// had, if any, to `taking`: copy its agents' sessions into its attached
    /// files, stop `leaving` and discard what it saved, record the handoff,
    /// and save `taking` as the space's stopped agent, told where the
    /// copies are, which starts at once unless `held` keeps it for `shoal
    /// resume`.
    pub async fn hand_off(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        leaving: Option<Leaving>,
        taking: Taking,
        held: bool,
    ) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let space = self.space(&selector).await?;
        let listed = self.copy_sessions(daemon, &space).await?;
        self.retire_leaving(daemon, &space).await?;
        let settings = daemon.space_settings(&space).await?;
        let told = brief::told(&space, leaving.as_ref(), &taking.agent, &listed);
        if let Some(leaving) = &leaving {
            let from = HandedOverSession {
                agent: leaving.agent.clone(),
                session: leaving.session.clone(),
                integration: settings.integration(&leaving.agent),
            };
            let to = HandedOverSession {
                agent: taking.agent.clone(),
                session: None,
                integration: settings.integration(&taking.agent),
            };
            let space = id.to_owned();
            (self.store)
                .write(move |tx| crate::store::agent_handoffs::record(tx, &space, &from, &to))
                .await?;
        }
        let origin = leaving.as_ref().and_then(|leaving| leaving.origin.clone());
        let execution =
            (leaving.map(|leaving| leaving.execution)).unwrap_or_else(ExecutionId::generate);
        taking
            .saved(execution.clone(), origin.clone(), told)
            .write(&self.paths, id)?;
        self.signals.agent_changed();
        if held {
            return Ok(());
        }
        let env = crate::terminal::detached_start_environment(origin.as_ref());
        (daemon
            .start_stopped_restore(&selector, &execution, env)
            .await)
            .map(drop)
    }

    /// Stop the agent of `space` that leaves, keeping nothing of it, and
    /// discard what a stop saved of it, so that the agent taking over is
    /// its only one.
    async fn retire_leaving(&self, daemon: &dyn Daemon, space: &crate::model::Space) -> Result<()> {
        let request = StopRequest::handoff();
        (daemon.stop_space_agent_executions(&space.id, &request)).await?;
        daemon.discard_replaced_agent(space).await
    }
}

impl Taking {
    /// This agent as the stopped agent of a space, saved as execution
    /// `execution` that ran at `origin`, which starts a new session told
    /// `told`.
    fn saved(self, execution: ExecutionId, origin: Option<Origin>, told: String) -> LastSession {
        LastSession {
            execution,
            agent: self.agent,
            session: None,
            moved: false,
            copied: None,
            converted: false,
            handoff: Some(HandoffBrief {
                told,
                note: self.note,
            }),
            origin,
            stop: None,
            turn: None,
            model: self.model,
            mode: self.mode,
            narrowing: self.narrowing,
        }
    }
}
