//! Continuing the turn an interrupt ended: the daemon queues a message for
//! the agent that wakes it, so the wrapper that runs it types its wake
//! prompt in the agent's terminal and the agent's hook delivers the message.
use anyhow::Result;

use crate::Agents;
use crate::daemon::Daemon;
use crate::model::{ResumeOutcome, RunningAgent, Space};
use crate::state::AgentState;
use shoal_daemon_base::alerts::AlertKind;
use shoal_protocol::space_names::SpaceSelector;

/// What the agent of an interrupted turn receives to continue it.
pub const CONTINUE_MESSAGE: &str = "Continue where you left off.";

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

    /// Continue the interrupted turn of `agent` in `space` with a message
    /// that wakes it.
    pub(super) async fn continue_turn(
        &self,
        space: &Space,
        agent: RunningAgent,
    ) -> Result<ResumeOutcome> {
        let message = CONTINUE_MESSAGE.to_owned();
        (self.queue_agent_message(&space.id, AlertKind::AgentResumed, message)).await?;
        Ok(ResumeOutcome::Continued { agent: agent.agent })
    }
}
