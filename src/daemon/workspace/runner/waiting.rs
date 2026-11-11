//! The forge's waiting jobs as entries of the agent queue. Each poll adds
//! the jobs a repository's runner may take, with the run each belongs to and
//! the workspace that run links, and drops the entries of jobs the forge no
//! longer lists as waiting, which another runner took or a cancel ended.
use crate::client::repository_ids::RepositoryId;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use super::{Registration, runner_labels};
use crate::{client::space_names::SpaceName, model::SpaceKind};
use crate::{
    daemon::{log, workspace::Manager},
    forge::{
        ForgeRepo,
        ci::runner::{JobRun, Trigger},
    },
    model::{QueuePlace, QueuedJob, Repository},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// Read the forge's waiting jobs while one could start, and every
    /// [`INTERVAL`](crate::daemon::runner::INTERVAL) while none can, so that new jobs show in the queue and
    /// those another runner took leave it.
    pub(crate) async fn poll_waiting_jobs(&self) -> Result<()> {
        if self.runners.waiting_jobs_due() || self.job_could_start().await? {
            self.refresh_waiting_jobs().await?;
        }
        Ok(())
    }

    /// Bring the job entries of each repository whose runner is registered
    /// and enabled in line with the jobs its forge lists as waiting.
    pub(crate) async fn refresh_waiting_jobs(&self) -> Result<()> {
        self.runners.read_waiting_jobs();
        let config = self.config();
        for repository in self.repositories().await? {
            let Ok(settings) = self.repository_settings_from(&config, &repository.id).await else {
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
                .refresh_repository_jobs(&repository, &registration, &labels)
                .await
            {
                let name = crate::repository::name(&repository);
                log!("runner: the waiting jobs of {name}: {error:#}");
            }
        }
        Ok(())
    }

    /// Queue the jobs `repository`'s forge lists as waiting for `labels`
    /// that the runner has not queued, claimed or passed over, and drop the
    /// entries of those it no longer lists.
    async fn refresh_repository_jobs(
        &self,
        repository: &Repository,
        registration: &Registration,
        labels: &[String],
    ) -> Result<()> {
        let forge = ForgeRepo::resolve(&registration.forge_repository, &self.forge_hosts()).await?;
        let account = self.user_account();
        let waiting = forge.waiting_jobs(&account, labels).await?;
        let listed: HashSet<String> = waiting.iter().map(|job| job.handle.clone()).collect();
        let id = repository.id.clone();
        let known = self
            .store
            .write(move |tx| forget_unlisted_jobs(tx, &id, &listed))
            .await?;
        for job in waiting {
            if known.contains(&job.handle) {
                continue;
            }
            let run = forge.job_run(&account, job.run).await?;
            let workspace_id = self.run_workspace(&forge, &repository.id, &run).await?;
            let job = QueuedJob {
                job,
                run,
                workspace_id,
            };
            let id = repository.id.clone();
            self.store.write(move |tx| queue_job(tx, &id, &job)).await?;
        }
        Ok(())
    }

    /// The workspace of `repository_id` that links the PR `run` ran for, or
    /// owns the branch it ran on.
    pub(super) async fn run_workspace(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        run: &JobRun,
    ) -> Result<Option<SpaceId>> {
        let workspaces = self.list_workspaces().await?;
        let mut own = workspaces
            .into_iter()
            .filter(|workspace| workspace.repository_id == *repository_id);
        Ok(match &run.trigger {
            Trigger::Pull { number } => own.find(|workspace| {
                workspace
                    .links
                    .prs
                    .iter()
                    .any(|url| forge.pull(url).is_ok_and(|(linked, _)| linked == *number))
            }),
            Trigger::Branch(branch) => {
                own.find(|workspace| workspace.branch.as_deref() == Some(branch.as_str()))
            }
            Trigger::Other => None,
        }
        .map(|workspace| workspace.id))
    }
}

/// Drop `repository_id`'s waiting job entries and passed-over jobs whose
/// handles the forge no longer lists among `listed`, returning the handles
/// the runner still knows: queued, starting, claimed or passed over.
fn forget_unlisted_jobs(
    tx: &rusqlite::Transaction<'_>,
    repository_id: &RepositoryId,
    listed: &HashSet<String>,
) -> Result<HashSet<String>> {
    let entries = tx
        .prepare(
            "SELECT id,json_extract(job,'$.job.handle'),state FROM agent_queue
             WHERE repository_id=?1 AND kind=?2",
        )?
        .query_map(params![repository_id, QueueKind::Job], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, QueueState)>>>()?;
    let mut known = HashSet::new();
    for (id, handle, state) in entries {
        if state == QueueState::Queued && !listed.contains(&handle) {
            tx.execute("DELETE FROM agent_queue WHERE id=?1", [id])?;
        } else {
            known.insert(handle);
        }
    }
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
                params![repository_id, handle],
            )?;
        }
    }
    Ok(known)
}

/// Queue `job` behind the work already waiting, except that it goes ahead of
/// a waiting restore of the agent of the workspace its run links.
fn queue_job(
    tx: &rusqlite::Transaction<'_>,
    repository_id: &RepositoryId,
    job: &QueuedJob,
) -> Result<()> {
    let id = QueueEntryId::generate();
    let name = SpaceName::derive(
        SpaceKind::Workspace,
        &format!("ci-{}-{}", job.job.name, job.job.id),
    );
    tx.execute(
        "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job)
         VALUES (?1,?2,?3,?4,'',?5,?6,?7)",
        params![
            id,
            QueueKind::Job,
            repository_id,
            name,
            QueueState::Queued,
            crate::time::UnixTime::now(),
            serde_json::to_string(job)?,
        ],
    )?;
    let restore: Option<QueueEntryId> = match &job.workspace_id {
        Some(workspace) => rusqlite::OptionalExtension::optional(tx.query_row(
            "SELECT id FROM agent_queue WHERE workspace_id=?1 AND kind=?2 AND state=?3",
            params![workspace, QueueKind::Restore, QueueState::Queued],
            |row| row.get(0),
        ))?,
        None => None,
    };
    if let Some(restore) = restore {
        crate::daemon::workspace::agent_queue::place_entry(tx, &id, &QueuePlace::Before(restore))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
