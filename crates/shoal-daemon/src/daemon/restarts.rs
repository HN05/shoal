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
    daemon::{scope::Caller, workspace::Manager, workspace::restarted},
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
    /// of `space`, which `asked_by` asks for and which replaces the restart
    /// recorded for it: skip it, record it until the agent's turn ends,
    /// record it in the line, or begin it now.
    async fn decide_restart(
        &self,
        space: &Space,
        restart: AgentRestart,
        asked_by: Option<SpaceId>,
    ) -> Result<Decision> {
        let mut waiters = self.restarts.decisions.lock().await;
        let waiting = waiters.remove(&space.id).unwrap_or_default();
        self.forget_restart(&space.id).await?;
        let (agent, execution) = match self.restartable_agent(space).await? {
            Ok(found) => found,
            Err(skipped) => return Ok(answered(waiting, skipped)),
        };
        let recorded = Recorded {
            space_id: space.id.clone(),
            restart,
            asked_by,
        };
        if recorded.restart.wait && !self.agent_turn_ended(&space.id).await? {
            self.record_restart(&recorded).await?;
            let waits = RestartWait::Turn;
            return Ok(answered(waiting, RestartOutcome::Waits { agent, waits }));
        }
        if recorded.restart.sequential {
            let (waiter, answer) = oneshot::channel();
            let waiting = waiting.into_iter().chain([waiter]);
            waiters.entry(space.id.clone()).or_default().extend(waiting);
            self.line_up(recorded).await?;
            return Ok(Decision::Recorded(answer));
        }
        Ok(Decision::Restarting {
            answer: self.restart_now(&execution, recorded).await?,
            agent,
            waiting,
        })
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
    async fn restart_now(
        &self,
        execution: &ExecutionId,
        recorded: Recorded,
    ) -> Result<oneshot::Receiver<Result<Option<Origin>>>> {
        if recorded.asked_by.is_none() {
            self.record_agent_interaction(execution).await;
        }
        let restart = recorded.restart;
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
