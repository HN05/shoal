//! What an issue, PR or CI run shows now, read across items, reviews and CI:
//! the snapshots watches compare, a PR's status and item views.
pub mod status;
pub mod view;

use anyhow::Result;

use crate::{
    capability::CapabilityRequests,
    ci::{
        checks::Checks,
        run_view::{RunViewRequests, RunViewSteps},
        runs::Runs,
    },
    transport::account::Account,
    updates::{FailedJob, MAX_LISTED_JOBS, Snapshot},
};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    capability::Capability,
    ci::{
        JobState,
        run_action::Run,
        run_view::{RunJob, stopped_run_state},
    },
    items::reference::ItemNumber,
    numbers::{IssueNumber, PullNumber},
    updates::{SnapshotKind, SnapshotState},
};

/// How the forge reads an item's activity.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Activity {
    /// An issue's state and discussion.
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, discussion, reviews, mergeability and checks.
    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot>;
}

/// Reading the activity of issues, PRs and CI runs.
#[shoal_forge_derive::extension(pub ActivityRequests, pub(crate) ActivitySteps)]
impl ForgeRepo {
    /// The activity of `item`, reading again only what `known`, the
    /// activity already recorded of it, cannot have changed.
    pub async fn item_activity(
        &self,
        account: &Account,
        item: &shoal_forge_types::items::reference::ItemRef,
        known: Option<&Snapshot>,
    ) -> Result<Snapshot> {
        match item.number() {
            ItemNumber::Issue(number) => self.issue_activity(account, number).await,
            ItemNumber::Pr(number) => self.activity(account, number).await,
            ItemNumber::Run(_) => self.run_activity(account, item, known).await,
        }
    }

    pub async fn issue_activity(&self, account: &Account, number: IssueNumber) -> Result<Snapshot> {
        self.kind()
            .issue_activity(self, account, number)
            .await?
            .complete()
    }

    pub async fn activity(&self, account: &Account, number: PullNumber) -> Result<Snapshot> {
        self.kind()
            .pr_activity(self, account, number)
            .await?
            .complete()
    }

    /// The activity of the run `run`: its state and, once it failed, its
    /// failed jobs with the steps that failed where their logs say. A run
    /// `known` already holds failed at the same finish keeps them from it,
    /// without reading its jobs again.
    async fn run_activity(
        &self,
        account: &Account,
        run: &shoal_forge_types::items::reference::ItemRef,
        known: Option<&Snapshot>,
    ) -> Result<Snapshot> {
        let run = self.view_run(account, run).await?;
        // A run held for approval has yet to run, whatever state the forge
        // gives it, so its watch goes on.
        let state = match run.waiting {
            true => SnapshotState::Waiting,
            false => run.state.into(),
        };
        let mut snapshot = Snapshot::of(SnapshotKind::Run, Some(state));
        snapshot.finish = run.finish.clone();
        if state == SnapshotState::Failed {
            match known.filter(|known| snapshot.same_finish(known)) {
                Some(known) => snapshot.keep_failure(known),
                None => self.read_failure(account, &run, &mut snapshot).await?,
            }
        }
        Ok(snapshot)
    }

    /// Record on `snapshot` how the failed `run` stopped and its failed jobs.
    async fn read_failure(
        &self,
        account: &Account,
        run: &Run,
        snapshot: &mut Snapshot,
    ) -> Result<()> {
        let jobs = self.jobs_of(account, run).await?;
        snapshot.state = Some(stopped_run_state(&jobs).into());
        snapshot.failed_jobs = self.failed_jobs(account, run, &jobs).await;
        Ok(())
    }

    /// The failed jobs of `run` among its `jobs`, with the failing steps of
    /// those a run's update names; only their logs are read.
    async fn failed_jobs(&self, account: &Account, run: &Run, jobs: &[RunJob]) -> Vec<FailedJob> {
        let failed = jobs.iter().filter(|job| job.state == JobState::Failed);
        let mut failed_jobs = Vec::new();
        for (index, job) in failed.enumerate() {
            let step = match index < MAX_LISTED_JOBS {
                true => self.failed_job_step(account, run, job).await,
                false => None,
            };
            failed_jobs.push(FailedJob {
                name: job.name.clone(),
                step,
            });
        }
        failed_jobs
    }

    /// The step that failed in `job`, where the forge lists it or reads its
    /// log and the log names one.
    async fn failed_job_step(&self, account: &Account, run: &Run, job: &RunJob) -> Option<String> {
        let listed = self.kind().listed_failed_step(self, account, job.id).await;
        if listed.is_some() || !self.supports(Capability::ReadJobLogs) {
            return listed;
        }
        let log = self
            .kind()
            .run_job_log(self, account, run, job)
            .await
            .ok()?;
        self.kind().failed_step(&log)
    }
}

#[cfg(test)]
mod tests;
