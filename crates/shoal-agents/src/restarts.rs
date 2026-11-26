//! `shoal restart` requests: which agent of a space restarts, and the record of
//! each restart, which outlives the request and the daemon until the agent's
//! restore applies its options.
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use tokio::sync::oneshot;

use crate::Agents;
use crate::agent::Agent;
use crate::config::permissions::PermissionChange;
use crate::control::Requested;
use crate::control::restarted;
use crate::daemon::Daemon;
use crate::log;
use crate::model::AgentRestart;
use crate::model::RestartOutcome;
use crate::model::RestartWait;
use crate::model::Space;
use record::Recorded;
use shoal_core::sessions::Origin;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::space_names::SpaceSelector;

pub mod agent;
pub mod deferred;
pub mod line;
pub mod record;

pub use line::run as run_line;

/// What the daemon keeps of the restarts it took.
#[derive(Default)]
pub struct Restarts {
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
    pub fn acting(&self, id: &SpaceId) -> bool {
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

/// The restart of `agent`, the agent of the space `space`, that a request
/// began, recorded as `generation`, which answers once the agent runs again,
/// and the requests `waiting` for the one recorded before.
struct BegunNow {
    space: SpaceId,
    generation: String,
    agent: Agent,
    answer: oneshot::Receiver<Result<Option<Origin>>>,
    waiting: Vec<Waiter>,
}

/// What a request's decision leaves it to do.
enum Decision {
    Answered(RestartOutcome),
    /// Wait for the restart that began.
    Restarting(BegunNow),
    /// Wait for the daemon to act on the restart it recorded.
    Recorded(oneshot::Receiver<Result<RestartOutcome, String>>),
}

impl Agents {
    /// Restart the agent of `space` as `restart` says, for `caller`, which
    /// must hold what the restart gives the agent.
    pub async fn restart_space(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &SpaceSelector,
        restart: AgentRestart,
    ) -> Result<RestartOutcome> {
        let space = daemon.space(space).await?;
        if let Some(change) = &restart.change {
            let repository = space.repository_id.as_ref();
            (daemon.ensure_change_held(caller, &space, change, repository)).await?;
        }
        let asked_by = caller.map(|caller| caller.space_id.clone());
        match self.decide_restart(&space, restart, asked_by).await? {
            Decision::Answered(outcome) => Ok(outcome),
            Decision::Restarting(begun) => self.restarted_now(begun).await,
            Decision::Recorded(answer) => (answer.await)
                .map_err(|_| anyhow!("the daemon dropped the restart"))?
                .map_err(|reason| anyhow!(reason)),
        }
    }

    /// Decide what becomes of `restart` of the agent of `space`, which
    /// `asked_by` asks for and which supersedes the restart recorded for it,
    /// while holding the decisions. It may skip the restart, record it until
    /// the agent's turn ends or in the line, begin it now or with the restart
    /// under way, or record it until the agent that restart started runs. A
    /// recorded restart answers at once, since a long wait is the daemon's to
    /// keep.
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
            begun: false,
        };
        // A restart under way stopped the agent, whose turn is over.
        if !self.restarting(&execution).await {
            if recorded.restart.wait && !self.agent_turn_ended(&space.id).await? {
                self.record_restart(&recorded).await?;
                return Ok(answered(waiting, waits(agent, RestartWait::Turn)));
            }
            if recorded.restart.sequential {
                self.line_up(recorded).await?;
                return Ok(answered(waiting, waits(agent, RestartWait::Line)));
            }
        }
        match self.restart_now(&execution, &recorded).await? {
            (Requested::Answers(answer), generation) => Ok(Decision::Restarting(BegunNow {
                space: space.id.clone(),
                generation,
                agent,
                answer,
                waiting,
            })),
            (Requested::Settled, _) => {
                self.record_restart(&recorded).await?;
                Ok(match recorded.restart.wait {
                    true => answered(waiting, waits(agent, RestartWait::Turn)),
                    false => wait_for_recorded(&mut waiters, &space.id, waiting),
                })
            }
        }
    }

    /// Drop the restart the daemon keeps for later for the agent of
    /// `space`, answering the requests that wait for it; one under way goes
    /// on. Returns how the agent would have restarted, if anything was kept.
    pub async fn cancel_restart(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceSelector,
    ) -> Result<Option<AgentRestart>> {
        let space = daemon.space(space).await?;
        let mut waiters = self.restarts.decisions.lock().await;
        let id = space.id.clone();
        let cancelled = (self.store)
            .write(move |tx| record::take_kept(tx, &id))
            .await?;
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

    /// Begin `recorded`, the restart of the agent of its space, which runs as
    /// `execution`. It is recorded as begun first, with the generation the
    /// record took, so a daemon that exits before the agent's restore applies
    /// its options leaves them to the wrapper that reattaches. One that does
    /// not begin is forgotten again. The user's restart is their interaction
    /// with the agent.
    async fn restart_now(
        &self,
        execution: &ExecutionId,
        recorded: &Recorded,
    ) -> Result<(Requested, String)> {
        if recorded.asked_by.is_none() {
            self.record_agent_interaction(execution).await;
        }
        let generation = self.record_begun_restart(recorded).await?;
        let restart = recorded.restart.clone();
        match (self.begin_restart(execution, restart.in_place, restart.change)).await {
            Ok(requested) => Ok((requested, generation)),
            Err(error) => {
                (self.forget_ended_restart(&recorded.space_id, &generation)).await;
                Err(error)
            }
        }
    }

    /// What the restart of `begun` did once it ended or waits for the
    /// stopped agent's processes, answering the requests that waited for
    /// the restart it replaced too; once it ended, its record goes.
    async fn restarted_now(&self, begun: BegunNow) -> Result<RestartOutcome> {
        let outcome = agent::outcome(begun.agent, restarted(begun.answer).await);
        if ended(&outcome) {
            (self.forget_ended_restart(&begun.space, &begun.generation)).await;
        }
        answer_all(begun.waiting, &outcome);
        outcome
    }

    /// Record `recorded` as begun, returning the generation it takes.
    async fn record_begun_restart(&self, recorded: &Recorded) -> Result<String> {
        let recorded = recorded.clone();
        (self.store)
            .write(move |tx| record::put_begun(tx, &recorded))
            .await
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

    /// Forget the restart of `generation` recorded for the space `id`,
    /// which ended, unless a newer request replaced it.
    async fn forget_ended_restart(&self, id: &SpaceId, generation: &str) {
        let (space, ended) = (id.to_owned(), generation.to_owned());
        let forgotten = (self.store).write(move |tx| record::take_generation(tx, &space, &ended));
        if let Err(error) = forgotten.await {
            log!("ended restart of {id} not forgotten: {error:#}");
        }
    }
}

/// Whether the restart that `outcome` answered ended, rather than going on
/// until the stopped agent's processes exit and its restore applies its
/// options.
fn ended(outcome: &Result<RestartOutcome>) -> bool {
    !matches!(outcome, Ok(RestartOutcome::Waits { .. }))
}

/// `restart`, which supersedes `earlier`: when and how come from it, and
/// the permission changes of both merge, its own winning on a key both set.
fn superseding(earlier: Option<Recorded>, restart: AgentRestart) -> AgentRestart {
    let earlier = earlier.and_then(|earlier| earlier.restart.change);
    let change = PermissionChange::then(earlier, restart.change.clone());
    AgentRestart { change, ..restart }
}

/// That the restart of `agent` waits for what `waits` names.
fn waits(agent: Agent, waits: RestartWait) -> RestartOutcome {
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
