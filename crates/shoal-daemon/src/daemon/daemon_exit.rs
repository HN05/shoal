//! What becomes of tracked work as the daemon exits, which
//! `docs/design/daemon.md` decides for each kind of exit.
use super::{
    executions::{Save, StopRequest},
    manager::Manager,
    shutdown_stop::{HandoverWindow, Shutdown, StoppedSpaces},
};
use crate::daemon::{log, runner_service::JobStop};
use crate::protocol::{Exit, QueuedLaunches};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::HashSet;

/// What becomes of running work as the daemon exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Wrappers detach and reattach to the next daemon.
    Detach,
    /// Work stops as `shoal stop --all` stops it and stays down until
    /// `shoal resume`, as running chats do.
    Stop,
    /// Work stops saved first, with restores that need no user queued and
    /// running chats saved to come back, as before a system shutdown, but
    /// CI jobs detach.
    Save,
}

impl From<Exit> for Ending {
    fn from(exit: Exit) -> Self {
        match exit {
            Exit::Stop => Self::Stop,
            Exit::Restart => Self::Detach,
        }
    }
}

impl Manager {
    /// End running work as the daemon exits with `ending`, refusing new
    /// executions before the stop lists what runs, and return the spaces
    /// whose work it stopped. What still runs afterwards, such as work that
    /// failed to stop, detaches or stops as [`Manager::stop_remaining_work`]
    /// decides.
    pub(crate) async fn end_work(&self, ending: Ending) -> StoppedSpaces {
        match ending {
            Ending::Detach => StoppedSpaces::none(),
            Ending::Stop => {
                self.agents.executions().close().await;
                self.stop_all_for_exit().await
            }
            Ending::Save => {
                self.agents.executions().close().await;
                let window = HandoverWindow::Own(crate::service::HANDOVER_TIMEOUT);
                (self.agents)
                    .stop_for_shutdown_of(self, Shutdown::Daemon, window)
                    .await
            }
        }
    }

    /// Stop what still runs as a shutdown does, and count the spaces of what
    /// it stopped, which could not reattach, among those `stopped`.
    pub(crate) async fn stop_remaining_work(&self, stopped: &mut StoppedSpaces) {
        let cut_off = self.agents.stop_for_shutdown().await;
        self.count_stopped(stopped, cut_off).await;
    }

    /// Add the spaces `ids` to those `stopped`, each once; one that cannot
    /// be read leaves work stopped where the answer cannot name it.
    async fn count_stopped(&self, stopped: &mut StoppedSpaces, ids: HashSet<SpaceId>) {
        for id in ids {
            if stopped.spaces.iter().any(|space| space.id == id) {
                continue;
            }
            match self.spaces.space(&SpaceSelector::id(&id)).await {
                Ok(space) => stopped.spaces.push(space),
                Err(error) => {
                    log!("stop as the daemon exits: space {id} unavailable: {error:#}");
                    stopped.every = false;
                }
            }
        }
    }

    /// Stop what `shoal stop --all` stops, as it does, keeping stopped
    /// agents down until `shoal resume`, after a stop before a shutdown that
    /// is under way. Returns the spaces stopped.
    async fn stop_all_for_exit(&self) -> StoppedSpaces {
        let _shutdown_stop = self.agents.shutdown_stopped().lock().await;
        let moment = "before the Shoal daemon stopped";
        self.agents
            .stop_every_target(
                self,
                moment,
                |_| true,
                |workspace| async move {
                    // A CI job's runner would end once it saw its run cancelled,
                    // after the daemon has gone; stop it now, as removal does,
                    // also when the forge refused the cancellation.
                    if (self.runner.cancel_job_before_stopping(
                        self,
                        &workspace,
                        JobStop::DaemonStop,
                    ))
                    .await
                    {
                        let request = StopRequest {
                            save: Save::Everything,
                            ..StopRequest::shutdown("the Shoal daemon stopped")
                        };
                        return self
                            .agents
                            .stop_space(self, &SpaceSelector::id(&workspace), request)
                            .await;
                    }
                    (self.agents.stop_space_manually(
                        self,
                        &SpaceSelector::id(&workspace),
                        false,
                        QueuedLaunches::Keep,
                    ))
                    .await?;
                    Ok(())
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests;
