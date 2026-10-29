//! What becomes of tracked work as the daemon exits: a restart or an update
//! leaves it running, `shoal daemon stop` stops it as `shoal stop --all` does,
//! and a signal stops it saved first, since it may come from a shutdown.
use super::{Manager, shutdown_stop::Shutdown};
use crate::protocol::Exit;

/// What becomes of running work as the daemon exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Wrappers detach and reattach to the next daemon.
    Detach,
    /// Work stops as `shoal stop --all` stops it and stays down until
    /// `shoal resume`.
    Stop,
    /// Work stops saved first, with restores that need no user queued, as
    /// before a system shutdown.
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
    /// End running work as the daemon exits with `ending`. What still runs
    /// afterwards, such as work that failed to stop, detaches or stops as
    /// [`Manager::stop_for_shutdown`] decides.
    pub(crate) async fn end_work(&self, ending: Ending) {
        match ending {
            Ending::Detach => {}
            Ending::Stop => self.stop_all_for_exit().await,
            // A system shutdown already stopped the work and holds the queue.
            Ending::Save if self.agent_queue.suspended() => {}
            Ending::Save => self.stop_for_shutdown_of(Shutdown::Daemon).await,
        }
    }

    /// Stop what `shoal stop --all` stops, as it does, keeping stopped
    /// agents down until `shoal resume`.
    async fn stop_all_for_exit(&self) {
        self.stop_every_target("before the Shoal daemon stopped", |workspace| async move {
            self.stop_workspace_manually(&workspace, false).await
        })
        .await;
    }
}

#[cfg(test)]
mod tests;
