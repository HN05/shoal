//! The spaces a job's run is for: the workspace that links the run's PR or
//! owns the branch it ran on, and the chats that link its PR. Linking a job
//! lists it in their status; it places none of them above the job's own
//! workspace. Links change after a job is queued, so whatever reads the
//! spaces a job is for links it again first.
use crate::client::repository_ids::RepositoryId;
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        ci::runner::{JobRun, Trigger, WaitingJob},
        numbers::PullNumber,
    },
    model::{Chat, QueuedJob, SpaceLinks, Workspace},
    state::{QueueKind, QueueState},
};

/// The spaces as they are now, which a job's run can be for.
pub(super) struct Spaces {
    workspaces: Vec<Workspace>,
    chats: Vec<Chat>,
}

impl Manager {
    /// The spaces as they are now.
    pub(super) async fn spaces_now(&self) -> Result<Spaces> {
        Ok(Spaces {
            workspaces: self.list_workspaces().await?,
            chats: self.chats().await?,
        })
    }

    /// `job` of `run` in `repository_id`, linked to the spaces its run is
    /// for now.
    pub(super) async fn link_job(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        job: WaitingJob,
        run: JobRun,
    ) -> Result<QueuedJob> {
        let space_ids = self
            .spaces_now()
            .await?
            .run_spaces(forge, repository_id, &run);
        Ok(QueuedJob {
            job,
            run,
            space_ids,
            default_branch: false,
        })
    }

    /// The spaces each waiting job's run is for now, by its entry's ID.
    pub(crate) async fn waiting_job_spaces(&self) -> Result<HashMap<QueueEntryId, Vec<SpaceId>>> {
        let waiting = self.store.read(|tx| waiting_jobs(tx)).await?;
        let mut linked = HashMap::new();
        if waiting.is_empty() {
            return Ok(linked);
        }
        let spaces = self.spaces_now().await?;
        for (id, repository, mut job) in waiting {
            self.relink_job(&spaces, &repository, &mut job).await;
            linked.insert(id, job.space_ids);
        }
        Ok(linked)
    }

    /// Link `job` of `repository_id` again to the spaces its run is for
    /// among `spaces`; one whose run names no forge Shoal can resolve keeps
    /// the spaces it had.
    pub(super) async fn relink_job(
        &self,
        spaces: &Spaces,
        repository_id: &RepositoryId,
        job: &mut QueuedJob,
    ) {
        match self.item_forge(&job.run.url).await {
            Ok(forge) => job.space_ids = spaces.run_spaces(&forge, repository_id, &job.run),
            Err(error) => crate::daemon::log!("runner: {}: {error:#}", job.job.name),
        }
    }
}

/// Each waiting job's entry ID, repository and job.
fn waiting_jobs(db: &Connection) -> Result<Vec<(QueueEntryId, RepositoryId, QueuedJob)>> {
    let rows = db
        .prepare(
            "SELECT id,repository_id,job FROM agent_queue
             WHERE kind=?1 AND state=?2 AND job IS NOT NULL",
        )?
        .query_map(params![QueueKind::Job, QueueState::Queued], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(QueueEntryId, RepositoryId, String)>>>()?;
    let mut jobs = Vec::new();
    for (id, repository, job) in rows {
        jobs.push((id, repository, serde_json::from_str(&job)?));
    }
    Ok(jobs)
}

impl Spaces {
    /// The spaces of `repository_id` the run `run` of `forge` is for: the
    /// workspace first, then the chats.
    fn run_spaces(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        run: &JobRun,
    ) -> Vec<SpaceId> {
        let workspace = self.run_workspace(forge, repository_id, run);
        let chats = self.run_chats(forge, repository_id, run);
        workspace.into_iter().chain(chats).collect()
    }

    /// The workspace of `repository_id` that links the PR `run` ran for, or
    /// owns the branch it ran on.
    fn run_workspace(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        run: &JobRun,
    ) -> Option<SpaceId> {
        let mut own = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.repository_id == *repository_id);
        match &run.trigger {
            Trigger::Pull { number } => {
                own.find(|workspace| links_pull(forge, &workspace.links, *number))
            }
            Trigger::Branch(branch) => {
                own.find(|workspace| workspace.branch.as_deref() == Some(branch.as_str()))
            }
            Trigger::Other => None,
        }
        .map(|workspace| workspace.id.clone())
    }

    /// The chats of `repository_id`, or of no repository, that link the PR
    /// `run` ran for.
    fn run_chats<'a>(
        &'a self,
        forge: &'a ForgeRepo,
        repository_id: &'a RepositoryId,
        run: &JobRun,
    ) -> impl Iterator<Item = SpaceId> + 'a {
        let number = match run.trigger {
            Trigger::Pull { number } => Some(number),
            _ => None,
        };
        self.chats
            .iter()
            .filter(move |chat| {
                number.is_some_and(|number| {
                    chat.repository_id
                        .as_ref()
                        .is_none_or(|id| id == repository_id)
                        && links_pull(forge, &chat.links, number)
                })
            })
            .map(|chat| chat.id.clone())
    }
}

/// Whether `links` names the PR `number` of `forge`'s repository.
fn links_pull(forge: &ForgeRepo, links: &SpaceLinks, number: PullNumber) -> bool {
    links
        .prs
        .iter()
        .any(|url| url.repository() == forge && url.pull().is_ok_and(|linked| linked == number))
}
