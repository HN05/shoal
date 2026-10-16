//! Which workspaces and queued agents `shoal ls` lists.
use anyhow::Result;

use crate::{
    cli::{client, context::Context, list_status::ListStatus, ui},
    model::{QueueEntry, Workspace},
};

/// The `shoal ls` filters, with the repository resolved to its ID. No
/// statuses keeps every status.
pub(super) struct ListFilter {
    repository: Option<String>,
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
                let repos = client::repositories(&ctx.paths).await?;
                let selector = ui::repository_selector(selector)?;
                Some(
                    crate::forge::repository::select(&repos, selector)
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

    fn keeps_repository(&self, repository_id: &str) -> bool {
        self.repository
            .as_deref()
            .is_none_or(|id| id == repository_id)
    }

    fn keeps_status(&self, status: ListStatus) -> bool {
        self.statuses.is_empty() || self.statuses.contains(&status)
    }
}
