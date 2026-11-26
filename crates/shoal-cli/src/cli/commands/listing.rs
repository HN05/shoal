//! Which spaces and queued agents `shoal list` shows.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::directory::Listed;

use crate::{
    cli::{context::Context, list_status::ListStatus, ui},
    model::{Chat, QueueEntry, Workspace},
};

/// No statuses keeps every status.
pub(super) struct ListFilter {
    repository: Option<RepositoryId>,
    /// The origin of the filter's repository, which tells it on the other
    /// machines.
    origin: Option<String>,
    statuses: Vec<ListStatus>,
}

impl ListFilter {
    pub(super) async fn resolve(
        ctx: &Context,
        repository: Option<String>,
        statuses: Vec<ListStatus>,
    ) -> Result<Self> {
        let (repository, origin) = match repository {
            Some(selector) => {
                let repos = crate::client::repositories::list(&ctx.paths).await?;
                let selector = ui::repository_selector(selector)?;
                let repository = crate::repository::select(&repos, selector).await?;
                // A checkout whose origin cannot be read matches no other
                // machine's spaces, as their machines report them.
                let origin = (crate::repository::identity(&repository.source).await)
                    .ok()
                    .flatten();
                (Some(repository.id.clone()), origin)
            }
            None => (None, None),
        };
        Ok(Self {
            repository,
            origin,
            statuses,
        })
    }

    /// `shown` are the statuses the workspace's row shows, one of which
    /// the status filter must keep.
    pub(super) fn keeps_workspace(&self, workspace: &Workspace, shown: &[ListStatus]) -> bool {
        self.keeps_repository(workspace.repository())
            && shown.iter().any(|status| self.keeps_status(*status))
    }

    pub(super) fn keeps_entry(&self, entry: &QueueEntry) -> bool {
        self.keeps_space_repository(entry.repository_id.as_ref())
            && self.keeps_status(ListStatus::of_entry(entry))
    }

    /// An agent queued in one of the `listed` workspaces, whose row names it,
    /// or one the filters keep by its own status, which lists in a row of its
    /// own. Every caller sees every entry.
    pub(super) fn keeps_listed_entry(&self, entry: &QueueEntry, listed: &[Workspace]) -> bool {
        let in_listed = |workspace: &SpaceId| listed.iter().any(|w| &w.id == workspace);
        entry.space().is_some_and(in_listed) || self.keeps_entry(entry)
    }

    /// Another machine's space, whose repository the filter matches by its
    /// origin.
    pub(super) fn keeps_remote(&self, remote: &Listed) -> bool {
        let origin = (remote.space.repository.as_ref()).and_then(|origin| origin.origin.as_ref());
        let repository = match &self.repository {
            Some(_) => origin.is_some() && origin == self.origin.as_ref(),
            None => true,
        };
        repository && self.keeps_status(remote.space.status)
    }

    /// A chat without a repository is in none, which a repository filter
    /// leaves out.
    pub(super) fn keeps_chat(&self, chat: &Chat) -> bool {
        self.keeps_space_repository(chat.repository_id.as_ref())
            && self.keeps_status(ListStatus::of_chat(chat))
    }

    /// Whether the repository filter keeps a space of `repository`, or one
    /// without a repository, which only no filter keeps.
    fn keeps_space_repository(&self, repository: Option<&RepositoryId>) -> bool {
        match repository {
            Some(id) => self.keeps_repository(id),
            None => self.repository.is_none(),
        }
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
