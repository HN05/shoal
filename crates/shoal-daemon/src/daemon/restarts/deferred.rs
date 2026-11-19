//! Acting on the restarts the daemon recorded: once a waiting agent's turn
//! ends, in the line for sequential ones, and dropping one whose agent
//! stopped or exited first. A restart that fails with no request left to
//! answer reaches the coordinator that asked for it, or else the user.
use std::{collections::HashSet, sync::Arc};

use anyhow::Result;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use tokio::sync::oneshot;

use super::{Waiter, answer_all, record::Recorded};
use crate::{
    agent::Agent,
    daemon::{
        agents::{Requested, restarted},
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    model::{RestartOutcome, RunningAgent},
    sessions::Origin,
};
use shoal_protocol::space_names::SpaceSelector;

/// Where the agent of a recorded restart stands.
enum Standing {
    /// It waits for its turn to end, for its wrapper to come back, or for
    /// a restart under way.
    Waits,
    /// It restarts now.
    Due,
    /// It stopped or exited, which ends the restart.
    Gone,
}

/// What acting on a recorded restart began.
enum Begun {
    /// The agent restarts, answering once it runs again.
    Restarting(Agent, oneshot::Receiver<Result<Option<Origin>>>),
    /// What the daemon did instead.
    Ended(Result<RestartOutcome>),
}

impl Manager {
    /// Act on each recorded restart whose agent can restart now, or that
    /// it ended: one in the line joins it, and any other restarts at once;
    /// and answer the requests whose restart went with its space.
    pub(crate) async fn restart_finished_turns(self: &Arc<Self>) -> Result<()> {
        self.answer_orphaned_waiters().await?;
        let recorded = self.store.read(|db| super::record::all(db)).await?;
        let agents = self.running_agents().await?;
        for recorded in recorded {
            let id = &recorded.space_id;
            match self.standing(&recorded, &agents).await? {
                Standing::Waits => {}
                Standing::Due if recorded.restart.sequential => self.restarts.line.join(id),
                Standing::Due | Standing::Gone => {
                    let (manager, id) = (self.clone(), id.clone());
                    tokio::spawn(async move { manager.act_on_restart(&id).await });
                }
            }
        }
        Ok(())
    }

    /// Where the agent of `recorded` stands among `agents`.
    async fn standing(&self, recorded: &Recorded, agents: &[RunningAgent]) -> Result<Standing> {
        let id = &recorded.space_id;
        if self.restarts.acting(id) {
            return Ok(Standing::Waits);
        }
        // The record ends with the agent it is for, which a newer agent of
        // the space does not take over.
        let theirs: Vec<&RunningAgent> = (agents.iter())
            .filter(|agent| agent.ran_as(&recorded.execution))
            .collect();
        for agent in &theirs {
            if self.restarting(&agent.execution).await {
                return Ok(Standing::Waits);
            }
        }
        if theirs.is_empty() || theirs.iter().any(|agent| agent.stopped.is_some()) {
            return Ok(Standing::Gone);
        }
        // A wrapper reconnecting to a restarted daemon brings its agent back.
        if theirs.iter().all(|agent| agent.agent.is_none()) {
            return Ok(Standing::Waits);
        }
        let ended =
            !recorded.restart.wait || (self.turn_ended(id, |agent| agent.space_id == *id)).await?;
        Ok(if ended {
            Standing::Due
        } else {
            Standing::Waits
        })
    }

    /// Restart the agent of the space `id` as the restart recorded for it
    /// says, answering the requests that wait for it, or telling who asked
    /// when it fails and none is left to answer. The restart stays recorded
    /// until it ends, so a daemon that exits meanwhile leaves it to the next.
    /// Whether the agent runs again.
    pub(super) async fn act_on_restart(&self, id: &SpaceId) -> bool {
        let (recorded, generation, waiting, begun) = match self.begin_recorded_restart(id).await {
            Ok(Some(begun)) => begun,
            Ok(None) => return false,
            Err(error) => {
                log!("recorded restart of {id} not begun: {error:#}");
                return false;
            }
        };
        let outcome = match begun {
            Begun::Restarting(agent, answer) => {
                super::agent::outcome(agent, restarted(answer).await)
            }
            Begun::Ended(outcome) => outcome,
        };
        self.end_recorded_restart(id, &generation).await;
        let ran = matches!(outcome, Ok(RestartOutcome::Restarted { .. }));
        if !answer_all(waiting, &outcome)
            && let Err(error) = &outcome
        {
            self.report_failed_restart(&recorded, error).await;
        }
        ran
    }

    /// Begin the restart recorded for the space `id`, with the requests that
    /// wait for it, holding the decisions so no request decides on the space
    /// meanwhile; none once nothing is recorded, while the daemon carries it
    /// out already, while the turn it waits for goes on, as a newer request
    /// may record after the line took the space, or while the agent is
    /// still to run after the restart under way.
    async fn begin_recorded_restart(
        &self,
        id: &SpaceId,
    ) -> Result<Option<(Recorded, String, Vec<Waiter>, Begun)>> {
        let mut waiters = self.restarts.decisions.lock().await;
        if self.restarts.acting(id) {
            return Ok(None);
        }
        let space = id.to_owned();
        let found = (self.store).read(move |tx| super::record::find_generation(tx, &space));
        let Some((recorded, generation)) = found.await? else {
            return Ok(None);
        };
        let space = self.space(&SpaceSelector::id(id)).await?;
        let agent = match self.restartable_agent(&space).await? {
            Ok((agent, execution)) if !self.runs_as_recorded(&recorded, &execution).await? => {
                let reason = "the agent it was for no longer runs".to_owned();
                let agent = Some(agent);
                Err(RestartOutcome::Skipped { agent, reason })
            }
            agent => agent,
        };
        let begun = match agent {
            Err(skipped) => Begun::Ended(Ok(skipped)),
            Ok(_) if recorded.restart.wait && !self.agent_turn_ended(id).await? => {
                return Ok(None);
            }
            Ok((agent, execution)) => {
                let restart = recorded.restart.clone();
                match self
                    .begin_restart(&execution, restart.in_place, restart.change)
                    .await
                {
                    Ok(Requested::Answers(answer)) => Begun::Restarting(agent, answer),
                    Ok(Requested::Settled) => return Ok(None),
                    Err(error) => Begun::Ended(Err(error)),
                }
            }
        };
        self.restarts.set_acting(id, true);
        let (space, begun_generation) = (id.to_owned(), generation.clone());
        (self.store)
            .write(move |tx| super::record::begin(tx, &space, &begun_generation))
            .await?;
        let waiting = waiters.remove(id).unwrap_or_default();
        Ok(Some((recorded, generation, waiting, begun)))
    }

    /// Whether `execution`, which a restart of the space of `recorded` picks
    /// now, runs the agent `recorded` is for, as it did or since a restart.
    pub(super) async fn runs_as_recorded(
        &self,
        recorded: &Recorded,
        execution: &ExecutionId,
    ) -> Result<bool> {
        let agents = self.running_agents().await?;
        Ok((agents.iter())
            .any(|agent| agent.execution == *execution && agent.ran_as(&recorded.execution)))
    }

    /// Answer the requests that wait for a restart no longer recorded, which
    /// went with its space.
    async fn answer_orphaned_waiters(&self) -> Result<()> {
        let mut waiters = self.restarts.decisions.lock().await;
        let recorded: HashSet<SpaceId> = (self.store.read(|db| super::record::all(db)).await?)
            .into_iter()
            .map(|recorded| recorded.space_id)
            .collect();
        let orphaned: Vec<SpaceId> = (waiters.keys())
            .filter(|id| !recorded.contains(*id))
            .cloned()
            .collect();
        for id in orphaned {
            let skipped = RestartOutcome::Skipped {
                agent: None,
                reason: "the restart went with its space".into(),
            };
            answer_all(waiters.remove(&id).unwrap_or_default(), &Ok(skipped));
        }
        Ok(())
    }

    /// Forget the restart of `generation` recorded for the space `id`,
    /// which ended, unless a newer request replaced it, and let the daemon
    /// act on the space's restarts again.
    async fn end_recorded_restart(&self, id: &SpaceId, generation: &str) {
        let _decisions = self.restarts.decisions.lock().await;
        let (space, ended) = (id.to_owned(), generation.to_owned());
        let forgotten =
            (self.store).write(move |tx| super::record::take_generation(tx, &space, &ended));
        if let Err(error) = forgotten.await {
            log!("ended restart of {id} not forgotten: {error:#}");
        }
        self.restarts.set_acting(id, false);
        // A newer restart recorded meanwhile waits for the watcher.
        self.signals.agent_changed();
    }

    /// Tell the coordinator whose process asked for `recorded`, or else the
    /// user, that its agent did not restart, for `error`.
    async fn report_failed_restart(&self, recorded: &Recorded, error: &anyhow::Error) {
        let id = &recorded.space_id;
        let Ok(space) = self.space(&SpaceSelector::id(id)).await else {
            return;
        };
        let message = format!("The agent of {} did not restart: {error:#}", space.name);
        let told = match &recorded.asked_by {
            Some(asker) => self.tell_space(asker, message.clone()).await,
            None => false,
        };
        if !told {
            let about = About::space(&space.name, id);
            (self.notify_about(about, NotificationKind::RecoveryBlocked, message)).await;
        }
    }
}
