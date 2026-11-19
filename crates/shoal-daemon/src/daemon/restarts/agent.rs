//! Which agent of a space a restart restarts, or why it restarts none, and
//! what its restart did.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;

use crate::{
    agent::Agent,
    daemon::workspace::{Manager, RestartWaits},
    model::{RestartOutcome, RestartWait, RunningAgent, Space},
    sessions::Origin,
};

impl Manager {
    /// The agent of `space` that a restart restarts, with the execution it
    /// runs as, or why it restarts none: it has none, its wrapper is gone,
    /// nothing resumes its session, or protection stopped it and it waits
    /// for its restore.
    pub(super) async fn restartable_agent(
        &self,
        space: &Space,
    ) -> Result<Result<(Agent, ExecutionId), RestartOutcome>> {
        let agents = self.running_agents().await?;
        let (stopped, agents): (Vec<_>, Vec<_>) = (agents.into_iter())
            .filter(|agent| agent.space_id == space.id)
            .partition(|agent| agent.stopped.is_some());
        // A connected agent comes before an execution whose wrapper is gone.
        let agent = (agents.iter().find(|agent| agent.agent.is_some()))
            .or(agents.first())
            .cloned();
        let Some(agent) = agent else {
            return Ok(Err(skipped(None, absent_reason(space, &stopped))));
        };
        match restartable(space, &agent) {
            Ok(name) => Ok(Ok((name, agent.execution))),
            Err(reason) => Ok(Err(skipped(agent.agent, reason))),
        }
    }
}

/// What the restart of `agent` that `restarted` answered did: a restart
/// that goes on once the stopped agent's processes exit waits for them.
pub(super) fn outcome(agent: Agent, restarted: Result<Option<Origin>>) -> Result<RestartOutcome> {
    match restarted {
        Ok(place) => Ok(RestartOutcome::Restarted { agent, place }),
        Err(error) => match error.downcast::<RestartWaits>() {
            Ok(RestartWaits(issue)) => Ok(RestartOutcome::Waits {
                agent,
                waits: RestartWait::Processes { issue },
            }),
            Err(error) => Err(error),
        },
    }
}

fn skipped(agent: Option<Agent>, reason: String) -> RestartOutcome {
    RestartOutcome::Skipped { agent, reason }
}

/// Why `space` restarts no agent, when the only ones it has are those of
/// `stopped`, which protection stopped.
fn absent_reason(space: &Space, stopped: &[RunningAgent]) -> String {
    match stopped
        .iter()
        .find_map(|agent| stopped_reason(space, agent))
    {
        Some(reason) => reason,
        None => "no agent runs there".into(),
    }
}

/// Why `running`, the agent of `space`, does not restart when protection
/// stopped it: it waits for its restore, which `shoal queue start` starts
/// now.
fn stopped_reason(space: &Space, running: &RunningAgent) -> Option<String> {
    let protection = running.stopped.as_ref()?;
    let agent = (running.agent.as_ref())
        .map(|agent| agent.name())
        .unwrap_or("its agent");
    Some(format!(
        "{agent} is stopped ({}) and resumes once {}; restore it now with shoal queue start {}",
        protection.reason, protection.resumes_when, space.number
    ))
}

/// The agent `running`, of `space`, runs, or why it cannot restart.
fn restartable(space: &Space, running: &RunningAgent) -> Result<Agent, String> {
    let manual = format!("use shoal stop {0} and shoal restore {0}", space.number);
    match &running.agent {
        None => Err(format!(
            "execution {} is disconnected; {manual}",
            running.execution
        )),
        Some(agent) if !running.restorable => Err(format!(
            "{agent} has no command to resume its session; {manual}"
        )),
        Some(agent) => Ok(agent.clone()),
    }
}
