//! Stopping tracked work before the system or the daemon shuts down, as
//! `shoal stop --all` does, while the daemon and wrappers still run to save it.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::collections::HashSet;

use anyhow::Result;
use futures_util::future::join_all;

use super::{Manager, StopRequest};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        log,
        notifications::{About, NotificationKind},
    },
    execution::recovery::{self, Saved, SavedExt as _},
    model::PresentSpace,
    protocol::QueuedLaunches,
};

/// What shuts down, which the stop and the user's notifications name.
#[derive(Clone, Copy)]
pub(crate) enum Shutdown {
    System,
    /// A signal ends the daemon, as a logout or a service manager's stop sends.
    Daemon,
}

impl Shutdown {
    fn request(self) -> StopRequest {
        match self {
            Self::System => StopRequest::shutdown("the system is shutting down"),
            Self::Daemon => StopRequest::shutdown("the Shoal daemon is stopping"),
        }
    }

    /// When the work stopped, as the user's notifications say it.
    fn moment(self) -> &'static str {
        match self {
            Self::System => "before the system shut down",
            Self::Daemon => "before the Shoal daemon stopped",
        }
    }

    fn notification(self) -> NotificationKind {
        match self {
            Self::System => NotificationKind::SystemShutdown,
            Self::Daemon => NotificationKind::DaemonStopped,
        }
    }
}

impl Manager {
    /// Stop the spaces `shoal stop --all` stops before the system shuts
    /// down. See [`Manager::stop_for_shutdown_of`].
    pub(crate) async fn stop_for_system_shutdown(&self) {
        self.stop_for_shutdown_of(Shutdown::System).await;
    }

    /// Stop the spaces `shoal stop --all` stops, each wrapper saving its
    /// agent before the stop and requesting restores that need no user,
    /// which the stop keeps with those already waiting. Hold the agent queue
    /// until a system shutdown is cancelled, so nothing starts again before
    /// it. The user learns what to continue. An update waits for the stop.
    /// Once a stop before a shutdown finished, later ones wait for it and
    /// leave the work alone.
    pub(crate) async fn stop_for_shutdown_of(&self, shutdown: Shutdown) {
        let mut finished = self.shutdown_stopped.lock().await;
        if *finished {
            return;
        }
        let stopped = self
            .stop_every_target(shutdown.moment(), |space| async move {
                // A CI job's run ends on its forge, before its runner stops.
                self.cancel_job_before_stopping(&space).await;
                self.stop_keeping_restores(&SpaceSelector::id(&space), shutdown.request())
                    .await
            })
            .await;
        self.notify_shutdown_stop(shutdown, &stopped).await;
        *finished = true;
    }

    /// Admit queued agents again once the shutdown was cancelled, and stop
    /// work again before the next one.
    pub(crate) async fn system_shutdown_cancelled(&self) {
        *self.shutdown_stopped.lock().await = false;
        self.agent_queue.suspend(false).await;
    }

    /// Stop each space `shoal stop --all` stops with `stop`, given its ID,
    /// in parallel, holding the agent queue so nothing starts again, and
    /// tell the user of each that failed to stop `moment`. Returns those
    /// stopped.
    pub(super) async fn stop_every_target<F, Fut>(&self, moment: &str, stop: F) -> Vec<PresentSpace>
    where
        F: Fn(SpaceId) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _operation = self.gates.operation().await;
        self.agent_queue.suspend(true).await;
        let targets = match self.stop_targets(QueuedLaunches::Keep).await {
            Ok(targets) => targets,
            Err(error) => {
                log!("stop {moment}: running spaces unavailable: {error:#}");
                return Vec::new();
            }
        };
        let results = join_all(targets.iter().map(|space| stop(space.id().clone()))).await;
        let mut stopped = Vec::new();
        for (space, result) in targets.into_iter().zip(results) {
            match result {
                Ok(()) => stopped.push(space),
                Err(error) => self.notify_stop_failed(moment, &space, &error).await,
            }
        }
        stopped
    }

    async fn notify_stop_failed(&self, moment: &str, space: &PresentSpace, error: &anyhow::Error) {
        log!("stop {moment}: {} not stopped: {error:#}", space.name());
        self.notify_about(
            About::space(space.name(), space.id()),
            NotificationKind::StopFailed,
            format!("Could not stop tracked executions {moment}: {error:#}"),
        )
        .await;
    }

    /// Tell the user what the stop of `stopped` saved and how it continues.
    async fn notify_shutdown_stop(&self, shutdown: Shutdown, stopped: &[PresentSpace]) {
        if stopped.is_empty() {
            return;
        }
        let queued = self
            .queued_stopped_restores()
            .await
            .unwrap_or_else(|error| {
                log!("shutdown stop: agent queue unavailable: {error:#}");
                HashSet::new()
            });
        let waiting: Vec<&PresentSpace> = stopped
            .iter()
            .filter(|space| {
                waits_for_resume(&self.paths, space, &queued).unwrap_or_else(|error| {
                    log!(
                        "shutdown stop: stopped work of {} unreadable: {error:#}",
                        space.name()
                    );
                    true
                })
            })
            .collect();
        let continues = continuation(&waiting);
        let count = stopped.len();
        let spaces = if count == 1 { "space" } else { "spaces" };
        let moment = shutdown.moment();
        self.notify(
            None,
            shutdown.notification(),
            format!("Stopped work in {count} {spaces} {moment}; {continues}"),
        )
        .await;
    }
}

/// Whether the space saved work only `shoal resume` continues: a command,
/// or an agent whose restore is neither `queued` nor requested. A done
/// space's requests wait for `shoal resume` to name it.
fn waits_for_resume(
    paths: &crate::paths::Paths,
    space: &PresentSpace,
    queued: &HashSet<ExecutionId>,
) -> Result<bool> {
    let saved = Saved::load(paths, space.id())?;
    let requested: HashSet<ExecutionId> = if space.done() {
        HashSet::new()
    } else {
        recovery::restore_requests(paths, space.id())?
            .into_iter()
            .map(|(execution, _)| execution)
            .collect()
    };
    Ok(!saved.commands.is_empty()
        || saved
            .agents
            .iter()
            .any(|agent| !requested.contains(&agent.id) && !queued.contains(&agent.id)))
}

/// How the work of the `waiting` spaces continues. `shoal resume --all`
/// skips done spaces, so they are named.
fn continuation(waiting: &[&PresentSpace]) -> String {
    let (done, open): (Vec<&PresentSpace>, Vec<&PresentSpace>) =
        waiting.iter().partition(|space| space.done());
    let mut commands = Vec::new();
    if !open.is_empty() {
        commands.push("shoal resume --all".to_owned());
    }
    if !done.is_empty() {
        let numbers: Vec<String> = done
            .iter()
            .map(|space| space.number().to_string())
            .collect();
        commands.push(format!("shoal resume {}", numbers.join(" ")));
    }
    if commands.is_empty() {
        "the agent queue restores its agents".into()
    } else {
        format!("continue it with {}", commands.join(" and "))
    }
}

#[cfg(test)]
mod tests;
