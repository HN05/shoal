//! `shoal queue move` of a running agent: its wrapper stops it as after a
//! protection stop and restores it in the same terminal once the agent queue
//! starts its restore, which needs only a free slot.
use anyhow::{Context, Result, bail, ensure};

use super::{Agent, Manager};
use crate::{execution::recovery, model::Workspace, protocol::Protection};

impl Manager {
    /// The execution of the one agent running in `workspace`, refusing one
    /// that cannot restore in place, as `shoal restart` does.
    pub(crate) async fn movable_agent(&self, workspace: &Workspace) -> Result<String> {
        let name = &workspace.name;
        let agents: Vec<_> = self
            .running_agents()
            .await?
            .into_iter()
            .filter(|running| running.workspace_id == workspace.id)
            .collect();
        let running = match agents.as_slice() {
            [running] => running,
            [] => bail!("{name} is not in the agent queue and runs no agent"),
            _ => bail!("{name} runs several agents; stop them with shoal stop {name}"),
        };
        let manual = format!("use shoal stop {name} and shoal resume {name}");
        let agent = running.agent.as_deref().with_context(|| {
            format!("execution {} is disconnected; {manual}", running.execution)
        })?;
        ensure!(
            running.restorable,
            "{agent} has no command to resume its session; {manual}"
        );
        Ok(running.execution.clone())
    }

    /// Stop the agent of `id` for its restore to wait in the agent queue,
    /// saving its record first, so a wrapper that dies stopping it leaves
    /// it for `shoal resume`.
    pub(crate) async fn stop_for_queue_move(&self, id: &str) -> Result<()> {
        self.executions
            .with_agents(|agents| self.stop_moved(id, agents.get_mut(id)))
            .await
    }

    fn stop_moved(&self, id: &str, agent: Option<&mut Agent>) -> Result<()> {
        let agent = agent.with_context(|| format!("no connected agent runs execution {id}"))?;
        ensure!(
            agent.recover,
            "{} has no command to resume its session; configure [agent_resume] for it",
            agent.name
        );
        ensure!(
            !*agent.stop.borrow() && agent.restart.is_none() && agent.overload.is_none(),
            "{} is already stopping",
            agent.name
        );
        let protection = Protection::queue_move();
        let handoff = recovery::save_record(
            &self.paths,
            &agent.workspace_id,
            id,
            &agent.name,
            Some(&protection.reason),
            agent.turn,
        )
        .context("save the agent's recovery record")?;
        agent.overload = Some(protection);
        if agent.stop.send(true).is_err() {
            agent.overload = None;
            recovery::consume(&handoff)?;
            bail!("{} disconnected", agent.name);
        }
        Ok(())
    }
}
