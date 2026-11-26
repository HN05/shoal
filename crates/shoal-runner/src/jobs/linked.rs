//! The spaces a job's run is for. Links change after a job is queued, so
//! whatever reads the spaces a job is for links it again first.
use std::collections::HashMap;

use anyhow::Result;
use shoal_forge_types::{
    ForgeRepo,
    ci::runner::{JobRun, Trigger, WaitingJob},
    numbers::PullNumber,
};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::model::{QueuedJob, Space, SpaceLinks};

use crate::{Runner, daemon::Daemon, log};

impl Runner {
    /// `job` of `run` in `repository_id`, linked to the spaces its run is
    /// for now.
    pub async fn link_job(
        &self,
        daemon: &dyn Daemon,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        job: WaitingJob,
        run: JobRun,
    ) -> Result<QueuedJob> {
        let spaces = daemon.present_spaces().await?;
        let space_ids = run_spaces(&spaces, forge, repository_id, &run);
        Ok(QueuedJob {
            job,
            run,
            space_ids,
            default_branch: false,
        })
    }

    /// The spaces each waiting job's run is for now, by its entry's ID.
    pub async fn waiting_job_spaces(
        &self,
        daemon: &dyn Daemon,
    ) -> Result<HashMap<QueueEntryId, Vec<SpaceId>>> {
        let records = self.records.clone();
        let waiting = self.store.read(move |tx| records.waiting_jobs(tx)).await?;
        let mut linked = HashMap::new();
        if waiting.is_empty() {
            return Ok(linked);
        }
        let spaces = daemon.present_spaces().await?;
        for (id, repository, mut job) in waiting {
            self.relink_job(&spaces, &repository, &mut job).await;
            linked.insert(id, job.space_ids);
        }
        Ok(linked)
    }

    /// Link `job` of `repository_id` again to the spaces its run is for
    /// among `spaces`; one whose run names no forge Shoal can resolve keeps
    /// the spaces it had.
    pub async fn relink_job(
        &self,
        spaces: &[Space],
        repository_id: &RepositoryId,
        job: &mut QueuedJob,
    ) {
        match self.forges.item_forge(&job.run.url).await {
            Ok(forge) => job.space_ids = run_spaces(spaces, &forge, repository_id, &job.run),
            Err(error) => log!("runner: {}: {error:#}", job.job.name),
        }
    }
}

/// The spaces among `spaces`, of `repository_id` or of no repository,
/// that the run `run` of `forge` is for, in their order there.
fn run_spaces(
    spaces: &[Space],
    forge: &ForgeRepo,
    repository_id: &RepositoryId,
    run: &JobRun,
) -> Vec<SpaceId> {
    (spaces.iter())
        .filter(|space| {
            (space.repository_id.as_ref()).is_none_or(|id| id == repository_id)
                && runs_for(space, forge, run)
        })
        .map(|space| space.id.clone())
        .collect()
}

/// Whether the run `run` of `forge` is for `space`: it links the PR the
/// run ran for, or its worktree has the branch it ran on.
fn runs_for(space: &Space, forge: &ForgeRepo, run: &JobRun) -> bool {
    match &run.trigger {
        Trigger::Pull { number } => links_pull(forge, &space.links, *number),
        Trigger::Branch(branch) => (space.worktree())
            .and_then(|worktree| worktree.branch())
            .is_some_and(|own| own.as_str() == branch.as_str()),
        Trigger::Other => false,
    }
}

fn links_pull(forge: &ForgeRepo, links: &SpaceLinks, number: PullNumber) -> bool {
    (links.prs.iter().map(|pr| &pr.url))
        .any(|url| url.repository() == forge && url.pull().is_ok_and(|linked| linked == number))
}
