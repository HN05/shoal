//! Continuing the turn an interrupt ended: the daemon submits a prompt to
//! continue in the place the agent runs in, through the host that keeps it,
//! after recording that the keys are Shoal's rather than the user's.
use anyhow::{Context as _, Result};

use crate::Agents;
use crate::daemon::Daemon;
use crate::model::{ResumeOutcome, RunningAgent, Space};
use crate::state::AgentState;
use shoal_core::terminal::CONTINUE_PROMPT;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// The agent of `space` that idles after an interrupt ended its turn,
    /// if one does.
    pub(super) async fn interrupted_agent(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<Option<RunningAgent>> {
        let selector = SpaceSelector::id(&space.id);
        let agents = self.space_agents(daemon, &selector).await?;
        Ok((agents.into_iter())
            .find(|agent| agent.interrupted && agent.state == Some(AgentState::Idle)))
    }

    /// Submit the prompt that continues the interrupted turn of `agent` in
    /// `space`, refusing one that runs in no place a host types into.
    pub(super) async fn continue_turn(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        agent: RunningAgent,
    ) -> Result<ResumeOutcome> {
        let name = (agent.agent.as_ref()).map_or("the agent", |agent| agent.name());
        let place = (agent.origin.clone()).with_context(|| {
            format!(
                "{name} in {} runs in no terminal session Shoal can type into",
                space.name
            )
        })?;
        let place = place.into_typed();
        let host = (self.session_hosts.iter())
            .find(|host| host.name() == place.host)
            .with_context(|| format!("terminal host {} is unavailable", place.host))?;
        let selector = SpaceSelector::id(&space.id);
        self.report_typing(daemon, &selector, &agent.execution)
            .await?;
        host.submit_prompt(&place, CONTINUE_PROMPT).await?;
        Ok(ResumeOutcome::Continued { agent: agent.agent })
    }
}
