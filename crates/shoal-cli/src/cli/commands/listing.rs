//! Which workspaces, chats and queued agents `shoal list` shows.
use crate::client::repository_ids::RepositoryId;
use anyhow::Result;

use crate::{
    cli::{context::Context, list_status::ListStatus, ui},
    model::{Chat, QueueEntry, Workspace},
};

/// The `shoal list` filters, with the repository resolved to its ID. No
/// statuses keeps every status.
pub(super) struct ListFilter {
    repository: Option<RepositoryId>,
    statuses: Vec<ListStatus>,
}

impl ListFilter {
    pub(super) async fn resolve(
        ctx: &Context,
        repository: Option<String>,
        statuses: Vec<ListStatus>,
    ) -> Result<Self> {
        let repository = match repository {
            Some(selector) => {
                let repos = crate::client::repositories::list(&ctx.paths).await?;
                let selector = ui::repository_selector(selector)?;
                Some(
                    crate::repository::select(&repos, selector)
                        .await?
                        .id
                        .clone(),
                )
            }
            None => None,
        };
        Ok(Self {
            repository,
            statuses,
        })
    }

    /// `stopped` says whether `shoal stop` saved the workspace's work.
    pub(super) fn keeps_workspace(&self, workspace: &Workspace, stopped: bool) -> bool {
        self.keeps_repository(&workspace.repository_id)
            && self.keeps_status(ListStatus::of_workspace(workspace, stopped))
    }

    pub(super) fn keeps_entry(&self, entry: &QueueEntry) -> bool {
        self.keeps_repository(&entry.repository_id)
            && self.keeps_status(ListStatus::of_entry(entry))
    }

    /// A chat without a repository is in none, which a repository filter
    /// leaves out.
    pub(super) fn keeps_chat(&self, chat: &Chat) -> bool {
        let repository = match &chat.repository_id {
            Some(id) => self.keeps_repository(id),
            None => self.repository.is_none(),
        };
        repository && self.keeps_status(ListStatus::of_chat(chat))
    }

    /// An item of `repository_id` without a workspace, such as an issue
    /// proposal that outlived it: kept by the repository filter, but never
    /// by a status filter, since it has no workspace status.
    pub(super) fn keeps_detached(&self, repository_id: &RepositoryId) -> bool {
        self.keeps_repository(repository_id) && self.statuses.is_empty()
    }

    /// An item of the daemon itself, which belongs to no repository and has
    /// no workspace status.
    pub(super) fn keeps_daemon(&self) -> bool {
        self.repository.is_none() && self.statuses.is_empty()
    }

    fn keeps_repository(&self, repository_id: &RepositoryId) -> bool {
        self.repository
            .as_ref()
            .is_none_or(|id| id == repository_id)
    }

    fn keeps_status(&self, status: ListStatus) -> bool {
        self.statuses.is_empty() || self.statuses.contains(&status)
    }
}
