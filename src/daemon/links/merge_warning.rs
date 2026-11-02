//! What merging a PR or landing a branch warns about: the sensitive files
//! that the last conflict check of the merged head found changed on both
//! sides. It reads that record and checks nothing again, so merging stays as
//! fast without one.
use anyhow::{Context, Result};

use crate::{
    daemon::workspace::{Manager, conflicts::recorded},
    forge::{ForgeRepo, items::link::ItemKind},
    git::{self, default_branch::DefaultBranchLookup},
    model::{ChangedOnBothSides, MergeWarning},
    protocol::ItemContext,
};

impl Manager {
    /// The warning for merging the PR `input` names in the repository of
    /// `context`, or the space's linked PR, as the workspaces that link it
    /// last checked its head.
    pub async fn merge_warning(
        &self,
        context: &ItemContext,
        input: Option<String>,
    ) -> Result<Option<MergeWarning>> {
        let (forge, number, url) = self.pull_to_merge(context, input).await?;
        let recorded = self.recorded_for_pull(url).await?;
        if recorded.is_empty() {
            return Ok(None);
        }
        let account = self.user_account();
        let current = forge.read(&account, ItemKind::Pr, number).await?;
        let head = current.head_sha.context("the forge names no PR head")?;
        Ok(warning(format!("PR #{number}"), recorded, &head))
    }

    /// The warning for landing the workspace's HEAD in the default branch,
    /// as its last check against that branch found it; a stacked
    /// workspace's check is against its base's branch instead.
    pub async fn land_warning(&self, selector: &str) -> Result<Option<MergeWarning>> {
        let workspace = self.workspace(selector).await?;
        let recorded = self.recorded_changed_on_both_sides(&workspace.id).await?;
        let Some(recorded) = recorded.filter(|recorded| !recorded.files.is_empty()) else {
            return Ok(None);
        };
        let repo = self.repository(&workspace.repository_id).await?;
        let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached);
        if recorded.target != default.await? {
            return Ok(None);
        }
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        Ok(warning(workspace.branch, vec![recorded], &head))
    }

    /// The forge, number and URL of the PR `input` names in the repository
    /// of `context`, or of the space's linked PR.
    async fn pull_to_merge(
        &self,
        context: &ItemContext,
        input: Option<String>,
    ) -> Result<(ForgeRepo, u64, String)> {
        let (forge, input) = match context {
            ItemContext::Workspace(selector) | ItemContext::Chat(selector) => {
                let space = self.link_space(selector).await?;
                let input = match input {
                    Some(input) => input,
                    None => self.linked_item(space.id(), ItemKind::Pr).await?,
                };
                (self.space_forge(&space).await?, input)
            }
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let input = input.context(super::actions::NOTHING_LINKED)?;
                (self.checkout_forge(&checkout).await?, input)
            }
        };
        let (number, url) = forge.pull(&input)?;
        Ok((forge, number, url))
    }

    /// The records of the workspaces that link the PR at `url` which list
    /// files changed on both sides.
    async fn recorded_for_pull(&self, url: String) -> Result<Vec<ChangedOnBothSides>> {
        self.store
            .run(move |db| {
                let mut found = Vec::new();
                for (id, record) in recorded::listing_files(db)? {
                    if links_pull(db, &id, &url)? {
                        found.push(record);
                    }
                }
                Ok(found)
            })
            .await
    }
}

/// Whether the workspace `id` links the PR at `url`.
fn links_pull(db: &rusqlite::Connection, id: &str, url: &str) -> Result<bool> {
    let links = super::wait::linked_items(db, id, Some(ItemKind::Pr))?;
    Ok(links.iter().any(|(linked, _)| linked == url))
}

/// The warning for merging `subject` at `head`, from the first of `recorded`
/// that the check made at that head.
fn warning(subject: String, recorded: Vec<ChangedOnBothSides>, head: &str) -> Option<MergeWarning> {
    let recorded = recorded
        .into_iter()
        .find(|recorded| recorded.head == head)?;
    Some(MergeWarning {
        subject,
        target: recorded.target,
        files: recorded.files,
    })
}

#[cfg(test)]
mod tests;
