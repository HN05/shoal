//! PR jobs whose PR moved to a newer head than their run's commit. Their
//! run is superseded, so the runner cancels it unless it finished, since
//! Shoal's runners leave its jobs waiting on the forge, where they would
//! hold the PR's checks, and passes the jobs over. A claim checks its job
//! before it starts.
use anyhow::Result;

use crate::client::space_names::SpaceName;
use crate::{
    daemon::{notifications::NotificationKind, workspace::Manager},
    forge::{ForgeRepo, ci::runner::JobRun, items::reference::ItemRef},
};

impl Manager {
    /// Whether `run`, of the job of the entry `name`, is for a PR that moved
    /// past its commit, cancelling the run when it is.
    pub(super) async fn cancel_if_superseded(
        &self,
        name: &SpaceName,
        forge: &ForgeRepo,
        run: &JobRun,
    ) -> Result<bool> {
        if !forge.run_superseded(&self.user_account(), run).await? {
            return Ok(false);
        }
        self.cancel_superseded(name, forge, &run.url).await;
        Ok(true)
    }

    /// Cancel the superseded `run` of the job of the entry `name` unless it
    /// finished. A cancel that fails tells the user.
    async fn cancel_superseded(&self, name: &SpaceName, forge: &ForgeRepo, run: &ItemRef) {
        let Err(error) = forge.cancel_unfinished(&self.user_account(), run).await else {
            return;
        };
        self.notify(
            Some(name),
            NotificationKind::RunnerFailed,
            format!(
                "CI run {run} is for a PR that moved to a newer head, and cancelling it \
                 failed: {error:#}. It may hold the PR's checks; cancel it with \
                 shoal ci cancel {run}"
            ),
        )
        .await;
    }
}

#[cfg(test)]
mod tests;
