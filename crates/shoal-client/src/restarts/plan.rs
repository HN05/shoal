//! Which agents a run restarts, in what order, and which it skips: those of
//! workspaces and of chats alike.
use anyhow::Result;

use crate::{
    connection::Daemon,
    model::{PendingRestart, PresentSpace, RunningAgent},
    output::agents::Skipped,
    state::AgentState,
};

/// The selected spaces and the agents that run there.
pub struct Selection {
    pub spaces: Vec<PresentSpace>,
    pub agents: Vec<RunningAgent>,
    /// The spaces were named, so one without an agent is reported.
    pub named: bool,
}

/// What a run restarts, in order, and what it skips.
pub struct Plan {
    pub restarts: Vec<Restart>,
    pub skipped: Vec<Skipped>,
    /// Names the run to the daemon, which records it on each restart.
    pub run: String,
}

/// One agent to restart.
pub struct Restart {
    /// The workspace or chat it runs in.
    pub space: PresentSpace,
    pub agent: RunningAgent,
    /// The restart in progress when the run selected it, which the run
    /// waits for instead of starting another, even once it ends.
    pub joins: Option<PendingRestart>,
}

impl Restart {
    /// Whether the agent can restart without interrupting a turn: it
    /// reported itself that its turn ended, since another agent's report in
    /// the same workspace does not count, or the run joins its restart.
    pub fn ready(&self) -> bool {
        self.agent.state == Some(AgentState::Idle) || self.joins.is_some()
    }

    /// The agent, skipped for `reason`.
    pub fn skipped(self, reason: impl Into<String>) -> Skipped {
        Skipped {
            space: self.space.name().clone(),
            agent: self.agent.agent,
            reason: reason.into(),
        }
    }
}

impl Selection {
    /// The agents named `agent` to restart, ready ones first, and those that
    /// cannot restart or already run again. Agents protection stopped wait
    /// for their restore and are left out, and reported as skipped when
    /// their space was named and runs no other.
    pub fn plan(self, agent: Option<&crate::agent::Agent>) -> Plan {
        let mut plan = Plan {
            restarts: Vec::new(),
            skipped: Vec::new(),
            run: uuid::Uuid::new_v4().to_string(),
        };
        for space in self.spaces {
            // An execution whose wrapper is gone may run any agent.
            let (stopped, agents): (Vec<_>, Vec<_>) = self
                .agents
                .iter()
                .filter(|running| running.space_id == *space.id())
                .filter(|running| {
                    agent.is_none() || running.agent.is_none() || running.agent.as_ref() == agent
                })
                .cloned()
                .partition(|running| running.stopped.is_some());
            if agents.is_empty() && self.named {
                plan.skipped.push(Skipped {
                    space: space.name().clone(),
                    agent: agent.cloned(),
                    reason: absent_reason(&space.number().to_string(), agent, &stopped),
                });
            }
            for running in agents {
                match skip_reason(&space, &running) {
                    Some(reason) => plan.skipped.push(Skipped {
                        space: space.name().clone(),
                        agent: running.agent,
                        reason,
                    }),
                    None => plan.restarts.push(Restart {
                        space: space.clone(),
                        joins: running.restart.clone(),
                        agent: running,
                    }),
                }
            }
        }
        plan.continue_earlier_run();
        plan.restarts.sort_by_key(|restart| !restart.ready());
        plan
    }
}

impl Plan {
    /// Restarts in progress come from an earlier run, which this one
    /// continues under its name: an agent that run restarted already runs
    /// again.
    fn continue_earlier_run(&mut self) {
        let runs: Vec<String> = self
            .restarts
            .iter()
            .filter_map(|restart| restart.agent.restart.as_ref())
            .flat_map(|pending| pending.run.iter().chain(&pending.joined).cloned())
            .collect();
        let Some(run) = runs.first() else {
            return;
        };
        self.run.clone_from(run);
        let (again, restarts) =
            std::mem::take(&mut self.restarts)
                .into_iter()
                .partition(|restart: &Restart| {
                    restart.agent.restart.is_none()
                        && restart
                            .agent
                            .restarted_by
                            .iter()
                            .any(|run| runs.contains(run))
                });
        self.restarts = restarts;
        self.skipped.extend(
            again
                .into_iter()
                .map(|restart| restart.skipped("the interrupted run restarted it already")),
        );
    }
}

/// The agents of workspaces and of chats, as a run selects and follows them.
pub async fn running_agents(daemon: &impl Daemon) -> Result<Vec<RunningAgent>> {
    crate::agents::all(daemon).await
}

/// Why `space`, a number, restarts no agent named `agent`, when the only ones
/// it has are those of `stopped`, which protection stopped.
fn absent_reason(
    space: &str,
    agent: Option<&crate::agent::Agent>,
    stopped: &[RunningAgent],
) -> String {
    if let Some(reason) = stopped
        .iter()
        .find_map(|running| stopped_reason(space, running))
    {
        return reason;
    }
    match agent {
        Some(agent) => format!("no {agent} agent runs there"),
        None => "no agent runs there".into(),
    }
}

/// Why `running`, the agent of `space`, does not restart when protection
/// stopped it: it waits for its restore, which `shoal queue start` starts
/// now.
pub(super) fn stopped_reason(space: &str, running: &RunningAgent) -> Option<String> {
    let protection = running.stopped.as_ref()?;
    let agent = running
        .agent
        .as_ref()
        .map(|agent| agent.name())
        .unwrap_or("its agent");
    Some(format!(
        "{agent} is stopped ({}) and resumes once {}; restore it now with shoal queue start {space}",
        protection.reason, protection.resumes_when
    ))
}

/// Why `running` cannot restart in place, if it cannot.
pub(super) fn skip_reason(space: &PresentSpace, running: &RunningAgent) -> Option<String> {
    let manual = format!("use shoal stop {0} and shoal restore {0}", space.number());
    match &running.agent {
        None => Some(format!(
            "execution {} is disconnected; {manual}",
            running.execution
        )),
        Some(agent) if !running.restorable => Some(format!(
            "{agent} has no command to resume its session; {manual}"
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests;
