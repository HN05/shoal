//! Which workspaces and queued agents `shoal ls` lists.
use anyhow::Result;

use crate::{
    cli::{client, context::Context, ui},
    model::{QueueEntry, Workspace},
};

/// The `shoal ls` filters, with the repository resolved to its ID.
pub(super) struct ListFilter {
    repository: Option<String>,
}

impl ListFilter {
    pub(super) async fn resolve(ctx: &Context, repository: Option<String>) -> Result<Self> {
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
        Ok(Self { repository })
    }

    pub(super) fn keeps_workspace(&self, workspace: &Workspace) -> bool {
        self.keeps_repository(&workspace.repository_id)
    }

    pub(super) fn keeps_entry(&self, entry: &QueueEntry) -> bool {
        self.keeps_repository(&entry.repository_id)
    }

    fn keeps_repository(&self, repository_id: &str) -> bool {
        self.repository
            .as_deref()
            .is_none_or(|id| id == repository_id)
    }
}
