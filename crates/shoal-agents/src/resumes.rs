//! `shoal resume` of a space's agent, which the space's daemon decides and
//! carries out. `docs/design/agent-lifecycle.md` says what a resume brings
//! back.
mod launches;
mod saved;
mod turns;

pub use turns::CONTINUE_MESSAGE;

use anyhow::{Result, ensure};

use crate::Agents;
use crate::daemon::Daemon;
use crate::model::{AgentResume, ResumeOutcome, Space};
use crate::state::WorkspaceState;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Resume the agent of the space `selector` names as `resume` says, for
    /// `caller`, whose scope the request was checked against; without
    /// executions, a turn an interrupt ended continues and a failed launch
    /// queues again before anything saved restores.
    pub async fn resume_space(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        resume: AgentResume,
    ) -> Result<ResumeOutcome> {
        let space = daemon.space(selector).await?;
        self.refuse_failed_setup(&space).await?;
        if resume.executions.is_empty() {
            if let Some(agent) = self.interrupted_agent(daemon, &space).await? {
                ensure!(
                    resume.change.is_none(),
                    "an interrupted turn continues in its running agent, which keeps its permissions; restart the agent with --permission instead"
                );
                return self.continue_turn(&space, agent).await;
            }
            if let Some(launch) = self.failed_launch_of(&space.id).await? {
                let outcome = self.retry_launch(daemon, caller, &space, launch, &resume);
                return outcome.await;
            }
        }
        self.restore_saved(daemon, caller, &space, resume).await
    }

    /// Refuse `space` when it is a workspace whose setup failed, or never
    /// started and runs in no execution, as when the daemon refused it,
    /// naming `shoal setup`, which retries it before the agent waiting for
    /// it starts; a chat has no setup.
    async fn refuse_failed_setup(&self, space: &Space) -> Result<()> {
        let Some(worktree) = space.worktree() else {
            return Ok(());
        };
        let awaits_setup = match worktree.state {
            WorkspaceState::Failed => true,
            WorkspaceState::Preparing => self.space_executions(&space.id).await?.is_empty(),
            _ => false,
        };
        ensure!(
            !awaits_setup,
            "the setup of {} failed; retry it with `shoal setup {}`",
            space.name,
            space.number
        );
        Ok(())
    }
}
