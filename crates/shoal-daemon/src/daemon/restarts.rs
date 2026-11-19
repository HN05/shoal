//! `shoal restart`: the daemon restarts the agent of the space a request
//! names, a workspace's or a chat's, which has at most one of its own, or
//! says why it does not. A restart that waits for the agent's turn to end,
//! or for its place among the sequential restarts, is recorded, outliving
//! the request and the daemon, and the daemon carries it out.
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use tokio::sync::oneshot;

use crate::{
    agent::Agent,
    config::permissions::PermissionChange,
    daemon::{
        agents::{Requested, restarted},
        manager::Manager,
        scope::Caller,
    },
    model::{AgentRestart, RestartOutcome, RestartWait, Space},
    sessions::Origin,
};
use record::Recorded;
use shoal_protocol::space_names::SpaceSelector;

mod agent;
mod deferred;
mod line;
pub(crate) mod record;

pub(crate) use line::run as run_line;

/// What the daemon keeps of the restarts it took.
#[derive(Default)]
pub(crate) struct Restarts {
    /// Held while a request or the daemon decides on a space's restart,
    /// with the requests that wait for the restart recorded for each space.
    decisions: tokio::sync::Mutex<HashMap<SpaceId, Vec<Waiter>>>,
    line: line::Line,
    /// The spaces whose recorded restart the daemon carries out now, which
    /// stays recorded until it ends, so a daemon that exits meanwhile leaves
    /// it to the next.
    acting: std::sync::Mutex<std::collections::HashSet<SpaceId>>,
}

impl Restarts {
    /// Whether the daemon carries out the restart recorded for the space
    /// `id` now.
    fn acting(&self, id: &SpaceId) -> bool {
        (self.acting.lock().unwrap_or_else(|e| e.into_inner())).contains(id)
    }

    /// Mark the restart recorded for the space `id` as carried out now, or
    /// as no longer when `acting` is false.
    fn set_acting(&self, id: &SpaceId, acting: bool) {
        let mut spaces = self.acting.lock().unwrap_or_else(|e| e.into_inner());
        match acting {
            true => spaces.insert(id.clone()),
            false => spaces.remove(id),
        };
    }
}

/// A request that waits for a recorded restart, answered with what the
/// daemon did or why it failed.
type Waiter = oneshot::Sender<Result<RestartOutcome, String>>;

/// What a request's decision leaves it to do.
enum Decision {
    Answered(RestartOutcome),
    /// Wait for the restart of `agent` that began, then answer the requests
    /// `waiting` for the one recorded before too.
    Restarting {
        agent: Agent,
        answer: oneshot::Receiver<Result<Option<Origin>>>,
        waiting: Vec<Waiter>,
    },
    /// Wait for the daemon to act on the restart it recorded.
    Recorded(oneshot::Receiver<Result<RestartOutcome, String>>),
}

impl Manager {
    /// Restart the agent of `space` as `restart` says, for `caller`, which
    /// must hold what the restart gives the agent.
    pub(crate) async fn restart_space(
        &self,
        caller: Option<&Caller>,
        space: &SpaceSelector,
        restart: AgentRestart,
    ) -> Result<RestartOutcome> {
        let space = self.space(space).await?;
        if let Some(change) = &restart.change {
            let giver = caller.map(|caller| &caller.space_id);
            let repository = space.repository_id.as_ref();
            (self.ensure_change_held(giver, &space, change, repository)).await?;
        }
        let asked_by = caller.map(|caller| caller.space_id.clone());
        match self.decide_restart(&space, restart, asked_by).await? {
            Decision::Answered(outcome) => Ok(outcome),
            Decision::Restarting {
                agent,
                answer,
                waiting,
            } => {
                let outcome = agent::outcome(agent, restarted(answer).await);
                answer_all(waiting, &outcome);
                outcome
            }
            Decision::Recorded(answer) => (answer.await)
                .map_err(|_| anyhow!("the daemon dropped the restart"))?
                .map_err(|reason| anyhow!(reason)),
        }
    }

    /// Decide, holding the decisions, what becomes of `restart` of the agent
    /// of `space`, which `asked_by` asks for and which supersedes the
    /// restart recorded for it: skip it, record it until the agent's turn
    /// ends, record it in the line, begin it now or with the restart under
    /// way, or record it until the agent that restart started runs.
    async fn decide_restart(
        &self,
        space: &Space,
        restart: AgentRestart,
        asked_by: Option<SpaceId>,
    ) -> Result<Decision> {
        let mut waiters = self.restarts.decisions.lock().await;
        let waiting = waiters.remove(&space.id).unwrap_or_default();
        let earlier = self.forget_restart(&space.id).await?;
        let (agent, execution) = match self.restartable_agent(space).await? {
            Ok(found) => found,
            Err(skipped) => return Ok(answered(waiting, skipped)),
        };
        let earlier = self.of_agent(earlier, &execution).await?;
        let recorded = Recorded {
            space_id: space.id.clone(),
            execution: execution.clone(),
            restart: superseding(earlier, restart),
            asked_by,
        };
        // A restart under way stopped the agent, whose turn is over.
        if !self.restarting(&execution).await {
            if recorded.restart.wait && !self.agent_turn_ended(&space.id).await? {
                self.record_restart(&recorded).await?;
                return Ok(answered(waiting, waits_for_turn(agent)));
            }
            if recorded.restart.sequential {
                self.line_up(recorded).await?;
                return Ok(wait_for_recorded(&mut waiters, &space.id, waiting));
            }
        }
        match self.restart_now(&execution, &recorded).await? {
            Requested::Answers(answer) => Ok(Decision::Restarting {
                agent,
                answer,
                waiting,
            }),
            Requested::Settled => {
                self.record_restart(&recorded).await?;
                Ok(match recorded.restart.wait {
                    true => answered(waiting, waits_for_turn(agent)),
                    false => wait_for_recorded(&mut waiters, &space.id, waiting),
                })
            }
        }
    }

    /// Drop the restart recorded for the agent of `space`, answering the
    /// requests that wait for it; one under way goes on. Returns how the
    /// agent would have restarted, if anything was recorded.
    pub(crate) async fn cancel_restart(
        &self,
        space: &SpaceSelector,
    ) -> Result<Option<AgentRestart>> {
        let space = self.space(space).await?;
        let mut waiters = self.restarts.decisions.lock().await;
        let cancelled = self.forget_restart(&space.id).await?;
        let waiting = waiters.remove(&space.id).unwrap_or_default();
        let skipped = RestartOutcome::Skipped {
            agent: None,
            reason: "the restart was cancelled".into(),
        };
        answer_all(waiting, &Ok(skipped));
        Ok(cancelled.map(|recorded| recorded.restart))
    }

    /// `earlier`, if it is a restart of the agent that runs as `execution`;
    /// one of an agent that exited ended with it.
    async fn of_agent(
        &self,
        earlier: Option<Recorded>,
        execution: &ExecutionId,
    ) -> Result<Option<Recorded>> {
        let Some(earlier) = earlier else {
            return Ok(None);
        };
        let same = self.runs_as_recorded(&earlier, execution).await?;
        Ok(same.then_some(earlier))
    }

    /// Whether the agent of the space `id` ended its turn.
    async fn agent_turn_ended(&self, id: &SpaceId) -> Result<bool> {
        self.turn_ended(id, |agent| agent.space_id == *id).await
    }

    /// Record `recorded`, the restart of the agent of its space, and put
    /// it in the line, since it waits for nothing else.
    async fn line_up(&self, recorded: Recorded) -> Result<()> {
        let id = recorded.space_id.clone();
        let restart = AgentRestart {
            wait: false,
            ..recorded.restart
        };
        (self.record_restart(&Recorded {
            restart,
            ..recorded
        }))
        .await?;
        self.restarts.line.join(&id);
        Ok(())
    }

    /// Begin `recorded`, the restart of the agent of its space, which runs
    /// as `execution`; the user's restart is their interaction with it.
    async fn restart_now(&self, execution: &ExecutionId, recorded: &Recorded) -> Result<Requested> {
        if recorded.asked_by.is_none() {
            self.record_agent_interaction(execution).await;
        }
        let restart = recorded.restart.clone();
        (self.begin_restart(execution, restart.in_place, restart.change)).await
    }

    async fn record_restart(&self, recorded: &Recorded) -> Result<()> {
        let recorded = recorded.clone();
        (self.store)
            .write(move |tx| record::put(tx, &recorded))
            .await?;
        // The turn ends watcher acts on it.
        self.signals.agent_changed();
        Ok(())
    }

    /// Forget the restart recorded for the space `id`, if it has one.
    async fn forget_restart(&self, id: &SpaceId) -> Result<Option<Recorded>> {
        let id = id.to_owned();
        self.store.write(move |tx| record::take(tx, &id)).await
    }
}

/// `restart`, which supersedes `earlier`: when and how come from it, and
/// the permission changes of both merge, its own winning on a key both set.
fn superseding(earlier: Option<Recorded>, restart: AgentRestart) -> AgentRestart {
    let earlier = earlier.and_then(|earlier| earlier.restart.change);
    let change = PermissionChange::then(earlier, restart.change.clone());
    AgentRestart { change, ..restart }
}

fn waits_for_turn(agent: Agent) -> RestartOutcome {
    let waits = RestartWait::Turn;
    RestartOutcome::Waits { agent, waits }
}

/// Have the request wait for the restart recorded for the space `id`, with
/// `waiting`, those that waited for the one it superseded.
fn wait_for_recorded(
    waiters: &mut HashMap<SpaceId, Vec<Waiter>>,
    id: &SpaceId,
    waiting: Vec<Waiter>,
) -> Decision {
    let (waiter, answer) = oneshot::channel();
    let waiting = waiting.into_iter().chain([waiter]);
    waiters.entry(id.clone()).or_default().extend(waiting);
    Decision::Recorded(answer)
}

/// Answer `waiting` and the request with `outcome`.
fn answered(waiting: Vec<Waiter>, outcome: RestartOutcome) -> Decision {
    answer_all(waiting, &Ok(outcome.clone()));
    Decision::Answered(outcome)
}

/// Answer each request of `waiting` with `outcome`, returning whether one
/// was still there to take it.
fn answer_all(waiting: Vec<Waiter>, outcome: &Result<RestartOutcome>) -> bool {
    let answer = || match outcome {
        Ok(outcome) => Ok(outcome.clone()),
        Err(error) => Err(format!("{error:#}")),
    };
    waiting
        .into_iter()
        .map(|waiter| waiter.send(answer()).is_ok())
        .filter(|&taken| taken)
        .count()
        > 0
}

#[cfg(test)]
mod tests;
