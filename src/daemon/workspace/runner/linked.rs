//! The spaces a job's run is for: the workspace that links the run's PR or
//! owns the branch it ran on, and the chats that link its PR. Linking a job
//! lists it in their status; it places none of them above the job's own
//! workspace.
use crate::client::repository_ids::RepositoryId;
use crate::client::space_ids::SpaceId;

use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        ci::runner::{JobRun, Trigger, WaitingJob},
        numbers::PullNumber,
    },
    model::{QueuedJob, SpaceLinks},
};

impl Manager {
    /// `job` of `run` in `repository_id`, linked to the spaces its run is
    /// for now.
    pub(super) async fn link_job(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        job: WaitingJob,
        run: JobRun,
    ) -> Result<QueuedJob> {
        let workspace_id = self.run_workspace(forge, repository_id, &run).await?;
        let chat_ids = self.run_chats(forge, repository_id, &run).await?;
        Ok(QueuedJob {
            job,
            run,
            workspace_id,
            chat_ids,
        })
    }

    /// The workspace of `repository_id` that links the PR `run` ran for, or
    /// owns the branch it ran on.
    async fn run_workspace(
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
            Trigger::Pull { number } => {
                own.find(|workspace| links_pull(forge, &workspace.links, *number))
            }
            Trigger::Branch(branch) => {
                own.find(|workspace| workspace.branch.as_deref() == Some(branch.as_str()))
            }
            Trigger::Other => None,
        }
        .map(|workspace| workspace.id))
    }

    /// The chats of `repository_id`, or of no repository, that link the PR
    /// `run` ran for.
    async fn run_chats(
        &self,
        forge: &ForgeRepo,
        repository_id: &RepositoryId,
        run: &JobRun,
    ) -> Result<Vec<SpaceId>> {
        let Trigger::Pull { number } = run.trigger else {
            return Ok(Vec::new());
        };
        Ok(self
            .chats()
            .await?
            .into_iter()
            .filter(|chat| {
                chat.repository_id
                    .as_ref()
                    .is_none_or(|id| id == repository_id)
                    && links_pull(forge, &chat.links, number)
            })
            .map(|chat| chat.id)
            .collect())
    }
}

/// Whether `links` names the PR `number` of `forge`'s repository.
fn links_pull(forge: &ForgeRepo, links: &SpaceLinks, number: PullNumber) -> bool {
    links
        .prs
        .iter()
        .any(|url| forge.pull(url).is_ok_and(|(linked, _)| linked == number))
}
