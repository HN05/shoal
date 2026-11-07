//! What becomes of tracked work as the daemon exits: a restart or an update
//! leaves it running, `shoal daemon stop` stops it as `shoal stop --all` does
//! but leaves queued launches to the next daemon and stops CI jobs' runners
//! after cancelling their runs, and a signal stops it saved first, since it
//! may come from a shutdown.
use super::{
    Manager,
    executions::{Save, StopRequest},
    shutdown_stop::Shutdown,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    protocol::{Exit, QueuedLaunches},
    state::ChatRestore,
};

/// What becomes of running work as the daemon exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Wrappers detach and reattach to the next daemon.
    Detach,
    /// Work stops as `shoal stop --all` stops it and stays down until
    /// `shoal resume`, as running chats do.
    Stop,
    /// Work stops saved first, with restores that need no user queued and
    /// running chats saved to come back, as before a system shutdown.
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
    /// executions before the stop lists what runs. What still runs
    /// afterwards, such as work that failed to stop, detaches or stops as
    /// [`Manager::stop_for_shutdown`] decides.
    pub(crate) async fn end_work(&self, ending: Ending) {
        match ending {
            Ending::Detach => {}
            Ending::Stop => {
                self.executions.close().await;
                self.stop_all_for_exit().await;
            }
            Ending::Save => {
                self.executions.close().await;
                self.stop_for_shutdown_of(Shutdown::Daemon).await;
            }
        }
    }

    /// Stop what `shoal stop --all` stops, as it does, keeping stopped
    /// agents down until `shoal resume`, and the running chats, saved for
    /// `shoal resume` too, after a stop before a shutdown that is under way.
    async fn stop_all_for_exit(&self) {
        let _shutdown_stop = self.shutdown_stopped.lock().await;
        tokio::join!(
            self.stop_every_target("before the Shoal daemon stopped", |workspace| async move {
                // A CI job's runner would end once it saw its run cancelled,
                // after the daemon has gone; stop it now, as removal does,
                // also when the forge refused the cancellation.
                if self.cancel_job_before_stopping(&workspace).await {
                    let request = StopRequest {
                        reason: "the Shoal daemon stopped".into(),
                        save: Save::Everything,
                    };
                    return self
                        .stop_workspace(&SpaceSelector::id(&workspace), request)
                        .await;
                }
                self.stop_workspace_manually(
                    &SpaceSelector::id(&workspace),
                    false,
                    None,
                    QueuedLaunches::Keep,
                )
                .await?;
                Ok(())
            }),
            self.save_chats(ChatRestore::Resume)
        );
    }
}

#[cfg(test)]
mod tests;
