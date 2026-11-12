//! When each agent of a run restarts: together or one at a time, with
//! `wait` only once its turn ends, and in turn idle ones first, each at least
//! the admission stagger after the previous start.
use std::time::Duration;

use anyhow::Result;
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::time::{Instant, sleep, sleep_until};

use super::plan::{Restart, running_agents, space_skip_reason, stopped_reason};
use crate::{
    agents,
    config::permissions::PermissionChange,
    connection::Daemon,
    executions::STAGGER,
    model::{PresentSpace, RunningAgent, Workspace},
    output::agents::Skipped,
    protocol::{ErrorCode, RemoteError},
    saved,
    sessions::Origin,
    space_names::{SpaceName, SpaceSelector},
};

/// How many agents restart at once, each holding one of the daemon's
/// long-lived connections until its agent runs again.
const PARALLEL_RESTARTS: usize = 16;

/// How often `wait` checks whether a turn ended.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How a run restarts the agents it selects.
#[derive(Clone)]
pub struct Options {
    /// Restart each agent only once its turn ends.
    pub wait: bool,
    /// Restart one agent at a time.
    pub sequential: bool,
    /// Restart each agent in its wrapper rather than a new one.
    pub in_place: bool,
    /// What changes in the permissions each restarted agent was given.
    pub change: Option<PermissionChange>,
}

/// What a caller does as a run goes on, such as telling the user.
pub trait Progress {
    /// The run does not restart an agent it selected.
    fn skipped(&mut self, skipped: Skipped);
    /// The restart of `restart` ended, with the place its agent runs in now
    /// when it runs again; [`waits`] tells a restart that goes on.
    fn ended(&mut self, restart: &Restart, result: Result<Option<Origin>>);
    /// The run waits for the turns of the agents of `spaces` to end.
    fn waiting(&mut self, spaces: &[&SpaceName]);
}

/// Restart `restarts` as `options` say, as part of `run`, telling
/// `progress` how each ends.
pub async fn restart_all(
    daemon: &impl Daemon,
    restarts: Vec<Restart>,
    run: &str,
    options: &Options,
    progress: &mut impl Progress,
) -> Result<()> {
    if options.sequential {
        restart_in_turn(daemon, progress, restarts, run, options).await
    } else {
        restart_together(daemon, progress, restarts, run, options).await
    }
}

/// What the restart waits for, when `error` answers a restart that goes on
/// once the stopped agent's remaining processes exit.
pub fn waits(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<RemoteError>()
        .filter(|remote| remote.code == ErrorCode::RestartWaits)
        .map(|remote| remote.message.as_str())
}

impl Restart {
    /// Restart the agent as `options` say, or join its restart, as part of
    /// `run`, returning the place it runs in now.
    async fn run(
        &self,
        daemon: &impl Daemon,
        run: &str,
        options: &Options,
    ) -> Result<Option<Origin>> {
        let run = Some(run.to_owned());
        let joins = self.joins.clone();
        let change = options.change.clone();
        match &self.space {
            PresentSpace::Chat(chat) => {
                agents::restart_chat(daemon, &chat.id, joins, run, change).await
            }
            PresentSpace::Workspace(workspace) => {
                self.restart_agent(daemon, workspace, run, options).await
            }
        }
    }

    /// [`Self::run`] for the agent of `workspace`, naming how to restore it
    /// when its restart failed.
    async fn restart_agent(
        &self,
        daemon: &impl Daemon,
        workspace: &Workspace,
        run: Option<String>,
        options: &Options,
    ) -> Result<Option<Origin>> {
        let (execution, joins) = (self.agent.execution.clone(), self.joins.clone());
        let (in_place, change) = (options.in_place, options.change.clone());
        match agents::restart(daemon, execution, joins, run, in_place, change).await {
            // A restart that waits still restores the agent itself, as a
            // wrapper waiting for its restore does.
            Err(error) if waits(&error).is_none() => {
                Err(with_restore(daemon, workspace, error).await)
            }
            restarted => restarted,
        }
    }
}

/// `error`, of the failed restart of `workspace`'s agent, naming how to
/// restore the agent when the workspace saved stopped work for that, as the
/// daemon names it for a chat. The daemon's error stays its cause, which
/// callers can inspect.
async fn with_restore(
    daemon: &impl Daemon,
    workspace: &Workspace,
    error: anyhow::Error,
) -> anyhow::Error {
    let saved = saved::read(daemon, SpaceSelector::id(&workspace.id)).await;
    match saved.is_ok_and(|saved| saved.is_resumable()) {
        true => error.context(format!(
            "the agent stays stopped; restore it with shoal resume {}",
            workspace.number
        )),
        false => error,
    }
}

/// Restart `restarts` one at a time, idle ones first, each at least the
/// admission stagger after the previous start, and with `wait` only once its
/// turn ended.
async fn restart_in_turn(
    daemon: &impl Daemon,
    progress: &mut impl Progress,
    mut restarts: Vec<Restart>,
    run: &str,
    options: &Options,
) -> Result<()> {
    let mut previous: Option<Instant> = None;
    while !restarts.is_empty() {
        if let Some(previous) = previous {
            sleep_until(previous + STAGGER).await;
        }
        let restart = if options.wait {
            let Some(restart) = next_idle(daemon, progress, &mut restarts, run).await? else {
                break;
            };
            restart
        } else {
            restarts.remove(0)
        };
        let result = restart.run(daemon, run, options).await;
        if result.is_ok() {
            previous = Some(Instant::now());
        }
        progress.ended(&restart, result);
    }
    Ok(())
}

/// Restart `restarts` together, up to [`PARALLEL_RESTARTS`] at a time, and
/// with `wait` each as soon as its turn ends.
async fn restart_together(
    daemon: &impl Daemon,
    progress: &mut impl Progress,
    mut waiting: Vec<Restart>,
    run: &str,
    options: &Options,
) -> Result<()> {
    let wait = options.wait;
    let mut running = FuturesUnordered::new();
    let mut told = false;
    loop {
        if wait {
            for gone in refresh(&mut waiting, &running_agents(daemon).await?, run) {
                progress.skipped(gone);
            }
        }
        while running.len() < PARALLEL_RESTARTS
            && let Some(index) = waiting.iter().position(|restart| !wait || restart.ready())
        {
            let restart = waiting.remove(index);
            running.push(async move {
                let result = restart.run(daemon, run, options).await;
                (restart, result)
            });
        }
        if waiting.is_empty() && running.is_empty() {
            return Ok(());
        }
        if wait && !told {
            told = tell_waiting(progress, &waiting);
        }
        tokio::select! {
            Some((restart, result)) = running.next() => progress.ended(&restart, result),
            () = sleep(POLL_INTERVAL), if wait && !waiting.is_empty() => {}
        }
    }
}

/// Tell `progress` whose turns the restarts wait for, returning whether any.
fn tell_waiting(progress: &mut impl Progress, restarts: &[Restart]) -> bool {
    let names: Vec<_> = restarts
        .iter()
        .filter(|restart| !restart.ready())
        .map(|restart| restart.space.name())
        .collect();
    if !names.is_empty() {
        progress.waiting(&names);
    }
    !names.is_empty()
}

/// Take the first of `restarts` whose turn ended, from states read now,
/// waiting until one has; an agent waiting for the user or without turn
/// reports works. Agents that stopped running, or that another command of
/// `run` restarted, are reported as skipped.
async fn next_idle(
    daemon: &impl Daemon,
    progress: &mut impl Progress,
    restarts: &mut Vec<Restart>,
    run: &str,
) -> Result<Option<Restart>> {
    let mut told = false;
    loop {
        for gone in refresh(restarts, &running_agents(daemon).await?, run) {
            progress.skipped(gone);
        }
        if restarts.is_empty() {
            return Ok(None);
        }
        if let Some(index) = restarts.iter().position(Restart::ready) {
            return Ok(Some(restarts.remove(index)));
        }
        if !told {
            told = tell_waiting(progress, restarts);
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Take each agent's turn state and restart from `running`, where a
/// restart may have started it in a new wrapper, and drop the agents that no
/// longer run, whose wrapper is gone, that protection stopped, or that
/// another command of `run` restarted meanwhile, returning them.
fn refresh(restarts: &mut Vec<Restart>, running: &[RunningAgent], run: &str) -> Vec<Skipped> {
    let mut ended = Vec::new();
    for mut restart in std::mem::take(restarts) {
        let Some(current) = running
            .iter()
            .find(|agent| agent.ran_as(&restart.agent.execution))
        else {
            ended.push(restart.skipped("the agent no longer runs"));
            continue;
        };
        let restarted = restart.joins.is_none() && current.restarted_by.iter().any(|by| by == run);
        let reason = stopped_reason(&restart.space.number().to_string(), current)
            .or_else(|| space_skip_reason(&restart.space, current));
        match reason {
            Some(reason) => ended.push(restart.skipped(reason)),
            None if restarted => {
                ended.push(restart.skipped("another command of this run restarted it already"));
            }
            None => {
                restart.agent = current.clone();
                restarts.push(restart);
            }
        }
    }
    ended
}

#[cfg(test)]
mod tests;
