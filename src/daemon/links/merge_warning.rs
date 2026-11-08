//! What merging a PR or a branch warns about: the sensitive files
//! that the last conflict check of the merged head found changed on both
//! sides. It reads that record and checks nothing again, so merging stays as
//! fast without one.
use crate::client::space_ids::SpaceId;
use anyhow::{Context, Result};

use crate::client::space_names::SpaceSelector;
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
    /// last checked its head against its base branch.
    pub async fn merge_warning(
        &self,
        context: &ItemContext,
        input: Option<String>,
    ) -> Result<Option<MergeWarning>> {
        let (forge, number, url) = self.pull_to_merge(context, input).await?;
        let recorded = self.recorded_for_pull(url.clone()).await?;
        if recorded.is_empty() {
            return Ok(None);
        }
        let account = self.user_account();
        let current = forge.read(&account, ItemKind::Pr, number).await?;
        let head = current.head_sha.context("the forge names no PR head")?;
        let base = current.item.pr.context("the forge names no PR base")?.base;
        let merged = Merged { head, target: base };
        let warning = warning(format!("PR #{number}"), recorded, &merged);
        Ok(warning.map(|warning| MergeWarning {
            url: Some(url),
            ..warning
        }))
    }

    /// The warning for merging the workspace's HEAD into the default branch,
    /// as its last check against that branch found it; a stacked
    /// workspace's check is against its base's branch instead.
    pub async fn branch_merge_warning(
        &self,
        selector: &SpaceSelector,
    ) -> Result<Option<MergeWarning>> {
        let workspace = self.workspace(selector).await?;
        let recorded = self.recorded_changed_on_both_sides(&workspace.id).await?;
        let Some(recorded) = recorded.filter(|recorded| !recorded.files.is_empty()) else {
            return Ok(None);
        };
        let repo = self.repository(&workspace.repository_id).await?;
        let target = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let merged = Merged { head, target };
        let branch = workspace.require_branch("merging")?.to_owned();
        Ok(warning(branch, vec![recorded], &merged))
    }

    /// The forge, number and URL of the PR `input` names in the repository
    /// of `context`, or of the space's linked PR.
    async fn pull_to_merge(
        &self,
        context: &ItemContext,
        input: Option<String>,
    ) -> Result<(ForgeRepo, u64, String)> {
        let (forge, input) = match context {
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let input = input.context(super::actions::NOTHING_LINKED)?;
                (self.checkout_forge(&checkout).await?, input)
            }
            _ => {
                let space = self.context_space(context).await?;
                let input = match input {
                    Some(input) => input,
                    None => self.linked_item(space.id(), ItemKind::Pr).await?,
                };
                (self.space_forge(&space).await?, input)
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
fn links_pull(db: &rusqlite::Connection, id: &SpaceId, url: &str) -> Result<bool> {
    let links = super::wait::linked_items(db, id, Some(ItemKind::Pr))?;
    Ok(links.iter().any(|(linked, _)| linked == url))
}

/// What a merge brings together: a head and the branch it merges into.
struct Merged {
    head: String,
    target: String,
}

/// The warning for merging `subject` as `merged`, from the first of
/// `recorded` that checked that head against that branch.
fn warning(
    subject: String,
    recorded: Vec<ChangedOnBothSides>,
    merged: &Merged,
) -> Option<MergeWarning> {
    let recorded = recorded
        .into_iter()
        .find(|recorded| recorded.head == merged.head && recorded.target == merged.target)?;
    Some(MergeWarning {
        subject,
        target: recorded.target,
        files: recorded.files,
        url: None,
    })
}

#[cfg(test)]
mod tests;
