//! PR jobs whose PR moved to a newer head than their run's commit. Their
//! run is superseded, so the runner cancels it unless it finished, since
//! Shoal's runners leave its jobs waiting on the forge, where they would
//! hold the PR's checks, and passes the jobs over. A claim checks its job
//! before it starts, and each waiting-jobs poll checks every queued one,
//! reading each PR's head once, so a job admission holds back does not hold
//! the PR's checks while it waits.
use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use shoal_daemon_base::notifications::About;
use shoal_forge::ci::{
    checks::CheckRequests, run_action::RunActionRequests, runner::RunnerRequests,
};
use shoal_forge_types::{
    ForgeRepo, ci::runner::JobRun, items::reference::ItemRef, numbers::PullNumber,
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{alerts::AlertKind, space_names::SpaceName};
use shoal_refs::CommitId;

use crate::{
    Runner,
    daemon::{Daemon, WaitingEntry},
    log,
};

impl Runner {
    /// Whether `run`, of the job of the entry `name`, is for a PR that moved
    /// past its commit, cancelling the run when it is.
    pub async fn cancel_if_superseded(
        &self,
        daemon: &dyn Daemon,
        name: &SpaceName,
        forge: &ForgeRepo,
        run: &JobRun,
    ) -> Result<bool> {
        if !forge.run_superseded(&daemon.user_account(), run).await? {
            return Ok(false);
        }
        self.cancel_superseded(daemon, name, forge, &run.url).await;
        Ok(true)
    }

    /// Cancel the runs of `repository_id`'s queued PR jobs whose PR moved
    /// past their commit, and pass the jobs over. A PR whose head cannot be
    /// read keeps its jobs queued until the next poll.
    pub async fn pass_over_superseded_jobs(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
        forge: &ForgeRepo,
    ) -> Result<()> {
        for (number, entries) in self.waiting_pull_jobs(repository_id).await? {
            let Some(head) = waiting_pull_head(daemon, forge, number).await else {
                continue;
            };
            let superseded = entries
                .into_iter()
                .filter(|entry| entry.job.run.superseded_by(&head))
                .collect();
            self.pass_over_superseded(daemon, repository_id, forge, superseded)
                .await?;
        }
        Ok(())
    }

    /// `repository_id`'s job entries waiting for admission whose runs are
    /// for a PR, by PR.
    async fn waiting_pull_jobs(
        &self,
        repository_id: &RepositoryId,
    ) -> Result<BTreeMap<PullNumber, Vec<WaitingEntry>>> {
        let (records, id) = (self.records.clone(), repository_id.to_owned());
        let entries = self
            .store
            .read(move |tx| records.waiting_job_entries(tx, &id))
            .await?;
        let mut by_pull = BTreeMap::<_, Vec<_>>::new();
        for entry in entries {
            if let Some(number) = entry.job.run.pull() {
                by_pull.entry(number).or_default().push(entry);
            }
        }
        Ok(by_pull)
    }

    /// Cancel the run of each of the superseded `entries` of
    /// `repository_id` once, and pass each over while it still waits for
    /// admission; one admitted since is left to its claim, which checks it
    /// again. Passing a job over keeps one whose run the forge did not
    /// cancel from joining the queue again.
    async fn pass_over_superseded(
        &self,
        daemon: &dyn Daemon,
        repository_id: &RepositoryId,
        forge: &ForgeRepo,
        entries: Vec<WaitingEntry>,
    ) -> Result<()> {
        let mut cancelled = HashSet::new();
        for entry in entries {
            if cancelled.insert(entry.job.run.url.clone()) {
                self.cancel_superseded(daemon, &entry.name, forge, &entry.job.run.url)
                    .await;
            }
            (self.pass_over_job(&entry.id, repository_id, &entry.job)).await?;
        }
        Ok(())
    }

    /// Cancel the superseded `run` of the job of the entry `name` unless it
    /// finished. A cancel that fails tells the user.
    async fn cancel_superseded(
        &self,
        daemon: &dyn Daemon,
        name: &SpaceName,
        forge: &ForgeRepo,
        run: &ItemRef,
    ) {
        let Err(error) = forge.cancel_unfinished(&daemon.user_account(), run).await else {
            return;
        };
        let message = format!(
            "CI run {run} is for a PR that moved to a newer head, and cancelling it \
             failed: {error:#}. It may hold the PR's checks; cancel it with \
             shoal ci cancel {run}"
        );
        let (about, kind) = (About::named(Some(name)), AlertKind::RunnerFailed);
        (self.notifications)
            .notify(daemon, about, kind, message, &[])
            .await;
    }
}

/// The head of PR `number`, whose jobs wait, or `None`, logged, when it
/// cannot be read.
async fn waiting_pull_head(
    daemon: &dyn Daemon,
    forge: &ForgeRepo,
    number: PullNumber,
) -> Option<CommitId> {
    let head = forge
        .numbered_pull_head(&daemon.user_account(), number)
        .await;
    head.inspect_err(|error| log!("runner: a PR's jobs stay queued: {error:#}"))
        .ok()
}
