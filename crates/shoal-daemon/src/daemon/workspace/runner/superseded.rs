//! PR jobs whose PR moved to a newer head than their run's commit. Their
//! run is superseded, so the runner cancels it unless it finished, since
//! Shoal's runners leave its jobs waiting on the forge, where they would
//! hold the PR's checks, and passes the jobs over. A claim checks its job
//! before it starts, and each waiting-jobs poll checks every queued one,
//! reading each PR's head once, so a job admission holds back does not hold
//! the PR's checks while it waits.
use crate::client::queue::QueueEntryId;
use crate::client::repository_ids::RepositoryId;
use std::collections::{BTreeMap, HashSet};

use anyhow::{Context as _, Result};
use rusqlite::params;
use shoal_refs::CommitId;

use crate::client::space_names::SpaceName;
use crate::{
    daemon::{log, notifications::NotificationKind, store::json_column, workspace::Manager},
    forge::{
        ForgeRepo,
        ci::runner::{JobRun, Trigger},
        items::{link::LinkKind, reference::ItemRef},
        numbers::PullNumber,
    },
    model::QueuedJob,
    state::{QueueKind, QueueState},
};

/// A job waiting in the queue for admission.
struct WaitingEntry {
    id: QueueEntryId,
    name: SpaceName,
    job: QueuedJob,
}

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

    /// Cancel the runs of `repository_id`'s queued PR jobs whose PR moved
    /// past their commit, and pass the jobs over. A PR whose head cannot be
    /// read keeps its jobs queued until the next poll.
    pub(super) async fn pass_over_superseded_jobs(
        &self,
        repository_id: &RepositoryId,
        forge: &ForgeRepo,
    ) -> Result<()> {
        for (number, entries) in self.waiting_pull_jobs(repository_id).await? {
            let Some(head) = self.waiting_pull_head(forge, number).await else {
                continue;
            };
            let superseded = entries
                .into_iter()
                .filter(|entry| entry.job.run.commit != head)
                .collect();
            self.pass_over_superseded(repository_id, forge, superseded)
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
        let id = repository_id.to_owned();
        let entries = self
            .store
            .read(move |tx| waiting_job_entries(tx, &id))
            .await?;
        let mut by_pull = BTreeMap::<_, Vec<_>>::new();
        for entry in entries {
            if let Trigger::Pull { number } = entry.job.run.trigger {
                by_pull.entry(number).or_default().push(entry);
            }
        }
        Ok(by_pull)
    }

    /// The head of PR `number`, whose jobs wait, or `None`, logged, when it
    /// cannot be read.
    async fn waiting_pull_head(&self, forge: &ForgeRepo, number: PullNumber) -> Option<CommitId> {
        let head = async {
            let pull = ItemRef::numbered(forge, LinkKind::Pr, number.get())?;
            let head = forge.pull_head(&self.user_account(), &pull).await;
            head.with_context(|| format!("read the head of {pull}"))
        };
        (head.await)
            .inspect_err(|error| log!("runner: a PR's jobs stay queued: {error:#}"))
            .ok()
    }

    /// Cancel the run of each of the superseded `entries` of
    /// `repository_id` once, and pass each over.
    async fn pass_over_superseded(
        &self,
        repository_id: &RepositoryId,
        forge: &ForgeRepo,
        entries: Vec<WaitingEntry>,
    ) -> Result<()> {
        let mut cancelled = HashSet::new();
        for entry in entries {
            if cancelled.insert(entry.job.run.url.clone()) {
                self.cancel_superseded(&entry.name, forge, &entry.job.run.url)
                    .await;
            }
            self.pass_over_waiting(repository_id, entry).await?;
        }
        Ok(())
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

    /// Leave the job of `entry` to other runners while it still waits for
    /// admission; one admitted since is left to its claim, which checks it
    /// again. Passing it over keeps a job whose run the forge did not
    /// cancel from joining the queue again.
    async fn pass_over_waiting(
        &self,
        repository_id: &RepositoryId,
        entry: WaitingEntry,
    ) -> Result<()> {
        let repository = repository_id.to_owned();
        self.store
            .write(move |tx| {
                let removed = tx.execute(
                    "DELETE FROM agent_queue WHERE id=?1 AND state=?2",
                    params![entry.id, QueueState::Queued],
                )?;
                if removed > 0 {
                    tx.execute(
                        "INSERT OR IGNORE INTO ci_passed_jobs(repository_id,handle)
                         VALUES (?1,?2)",
                        params![repository, entry.job.job.handle],
                    )?;
                }
                Ok(())
            })
            .await
    }
}

/// `repository_id`'s job entries waiting for admission.
fn waiting_job_entries(
    tx: &rusqlite::Transaction<'_>,
    repository_id: &RepositoryId,
) -> Result<Vec<WaitingEntry>> {
    let entries = tx
        .prepare(
            "SELECT id,name,job FROM agent_queue
             WHERE repository_id=?1 AND kind=?2 AND state=?3",
        )?
        .query_map(
            params![repository_id, QueueKind::Job, QueueState::Queued],
            |row| Ok((row.get("id")?, row.get("name")?, json_column(row, "job")?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries
        .into_iter()
        .filter_map(|(id, name, job)| {
            Some(WaitingEntry {
                id,
                name,
                job: job?,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests;
