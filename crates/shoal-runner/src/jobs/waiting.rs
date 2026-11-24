//! The forge's waiting jobs as entries of the agent queue. Each poll adds
//! the jobs a repository's runner may take, with the run each belongs to and
//! the spaces that run links, drops the entries of jobs the forge no longer
//! lists as waiting, which another runner took or a cancel ended, and passes
//! over the jobs of PRs that moved on.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;
use shoal_core::git::default_branch::{self, DefaultBranchLookup};
use shoal_forge::ci::runner::RunnerRequests;
use shoal_forge_types::ci::runner::{JobRun, Trigger};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::model::Repository;
use shoal_refs::BranchName;

use super::{Registration, runner_labels};
use crate::{Runner, daemon::Daemon, log};

impl Runner {
    /// Read the forge's waiting jobs while one could start, and every
    /// [`INTERVAL`](super::INTERVAL) while none can, so that new jobs show
    /// in the queue and those another runner took leave it.
    pub async fn poll_waiting_jobs(&self, daemon: &dyn Daemon) -> Result<()> {
        if self.jobs.waiting_jobs_due() || daemon.job_could_start().await? {
            self.refresh_waiting_jobs(daemon).await?;
        }
        Ok(())
    }

    /// Bring the job entries of each repository whose runner is registered
    /// and enabled in line with the jobs its forge lists as waiting.
    pub async fn refresh_waiting_jobs(&self, daemon: &dyn Daemon) -> Result<()> {
        self.jobs.read_waiting_jobs();
        let config = self.config();
        for repository in daemon.repositories().await? {
            let settings = daemon.repository_settings_from(&config, &repository.id);
            let Ok(settings) = settings.await else {
                continue;
            };
            if !settings.runner.enabled {
                continue;
            }
            let Some(registration) = self.runner_registration(&repository.id).await? else {
                continue;
            };
            let labels = runner_labels(&settings.runner.labels);
            if let Err(error) = self
                .refresh_repository_jobs(daemon, &repository, &registration, &labels)
                .await
            {
                let name = repository.display_name();
                log!("runner: the waiting jobs of {name}: {error:#}");
            }
        }
        Ok(())
    }

    /// Queue the jobs `repository`'s forge lists as waiting for `labels`
    /// that the runner has not queued, claimed or passed over, drop the
    /// entries of those it no longer lists, and pass over the superseded
    /// ones.
    async fn refresh_repository_jobs(
        &self,
        daemon: &dyn Daemon,
        repository: &Repository,
        registration: &Registration,
        labels: &[String],
    ) -> Result<()> {
        let forge = self.forges.forge(&registration.forge_repository).await?;
        let account = self.forges.user_account();
        let waiting = forge.waiting_jobs(&account, labels).await?;
        let listed: HashSet<String> = waiting.iter().map(|job| job.handle.clone()).collect();
        let (records, id) = (self.records.clone(), repository.id.clone());
        let known = self
            .store
            .write(move |tx| forget_unlisted_jobs(&*records, tx, &id, &listed))
            .await?;
        let default_branch = default_branch::resolve(&repository.path, DefaultBranchLookup::Cached)
            .await
            .ok();
        for job in waiting {
            if known.contains(&job.handle) {
                continue;
            }
            let run = forge.job_run(&account, job.run).await?;
            let linked = self.link_job(daemon, &forge, &repository.id, job, run);
            let mut job = linked.await?;
            job.default_branch =
                on_branch(&job.run, default_branch.as_ref().map(BranchName::as_str));
            let (records, id) = (self.records.clone(), repository.id.clone());
            self.store
                .write(move |tx| records.queue_job(tx, &id, &job))
                .await?;
        }
        (self.pass_over_superseded_jobs(daemon, &repository.id, &forge)).await
    }
}

/// Whether `run` is on `branch`, the repository's default branch when known.
fn on_branch(run: &JobRun, branch: Option<&str>) -> bool {
    matches!(&run.trigger, Trigger::Branch(on) if Some(on.as_str()) == branch)
}

/// Drop `repository_id`'s waiting job entries and passed-over jobs whose
/// handles the forge no longer lists among `listed`, returning the handles
/// the runner still knows: queued, starting, claimed or passed over.
fn forget_unlisted_jobs(
    records: &dyn crate::daemon::Records,
    tx: &Connection,
    repository_id: &RepositoryId,
    listed: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut known = records.forget_unlisted_job_entries(tx, repository_id, listed)?;
    // The forge lists a claimed job until its runner takes it.
    let claimed = tx
        .prepare("SELECT json_extract(job,'$.job.handle') FROM ci_jobs WHERE repository_id=?1")?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    known.extend(claimed);
    let passed = tx
        .prepare("SELECT handle FROM ci_passed_jobs WHERE repository_id=?1")?
        .query_map([repository_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    for handle in passed {
        if listed.contains(&handle) {
            known.insert(handle);
        } else {
            tx.execute(
                "DELETE FROM ci_passed_jobs WHERE repository_id=?1 AND handle=?2",
                rusqlite::params![repository_id, handle],
            )?;
        }
    }
    Ok(known)
}

#[cfg(test)]
mod tests;
