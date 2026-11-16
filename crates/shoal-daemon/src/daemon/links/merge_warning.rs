//! What merging a PR or a branch warns about: the sensitive files
//! that the last conflict check of the merged head found changed on both
//! sides. It reads that record and checks nothing again, so merging stays as
//! fast without one.
use anyhow::{Context, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

use crate::{
    daemon::workspace::{Manager, conflicts::recorded},
    forge::{
        ForgeRepo,
        items::{
            action::Action,
            item::Item,
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    git::{self, default_branch::DefaultBranchLookup},
    model::{ChangedOnBothSides, ConfirmedWarning, MergeWarning},
    protocol::ItemContext,
};
use shoal_forge::{RepoRequests, items::action::ActionRequests};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The warning for merging the PR `input` names in the repository of
    /// `context`, or the space's linked PR, as the workspaces that link it
    /// last checked its head against its base branch.
    pub async fn merge_warning(
        &self,
        context: &ItemContext,
        input: Option<ItemInput>,
    ) -> Result<Option<MergeWarning>> {
        let (forge, url) = self.context_pull(context, input).await?;
        let number = url.pull()?;
        let recorded = self.recorded_for_pull(url.clone()).await?;
        if recorded.is_empty() {
            return Ok(None);
        }
        let account = self.user_account();
        let current = forge.read(&account, ItemKind::Pr, number.get()).await?;
        let head = current.head_sha.context("the forge names no PR head")?;
        let base = current.item.pr.context("the forge names no PR base")?.base;
        let merged = Merged {
            head,
            target: base.into(),
        };
        let warning = warning(format!("PR #{number}"), recorded, &merged);
        Ok(warning.map(|warning| MergeWarning {
            url: Some(url),
            ..warning
        }))
    }

    /// Carry out `action`, a merge of the PR `input` names or of the
    /// space's linked one, unless its warning is no longer the one the user
    /// `confirmed`. The PR is resolved once, so a link that changes while
    /// the warning is read leaves the merge with the PR it checked.
    pub async fn confirmed_merge(
        &self,
        context: &ItemContext,
        input: Option<ItemInput>,
        action: Action,
        confirmed: &ConfirmedWarning,
        caller: Option<&crate::daemon::scope::Caller>,
    ) -> Result<Item> {
        ensure!(action.merges(), "only a merge confirms a merge warning");
        let (_, url) = self.context_pull(context, input).await?;
        let current = self
            .merge_warning(context, Some(url.clone().into()))
            .await?;
        ensure!(
            current == confirmed.warning,
            "the merge warning changed since you confirmed the merge; run shoal pr merge again"
        );
        self.item_action(context, ItemKind::Pr, Some(url.into()), action, caller)
            .await
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
        let repo = self.repository(workspace.repository()).await?;
        let target = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let merged = Merged {
            head,
            target: target.into(),
        };
        let branch = workspace.require_branch("merging")?.to_owned();
        Ok(warning(branch.into(), vec![recorded], &merged))
    }

    /// Bind a merge in `action` of the PR `input` names, or of `context`'s
    /// linked PR, to the commit the PR's head has now, unless it names one,
    /// and return what it does as an approval of it binds it: a later
    /// commit needs another decision, and the forge refuses to merge one.
    pub(crate) async fn bind_merge(
        &self,
        context: &ItemContext,
        input: Option<ItemInput>,
        action: &mut Action,
    ) -> Result<String> {
        let (forge, url) = self.context_pull(context, input).await?;
        if action.merges() && action.merge_head().is_none() {
            let pull = forge.pull_request(&self.agent_account(), &url).await?;
            let head = pull.head_commit.context("the forge names no PR head")?;
            action.bind_merge_head(head);
        }
        let at = (action.merge_head()).map_or_else(String::new, |head| format!(" at {head}"));
        Ok(format!("{} {url}{at}", action.name()))
    }

    /// The forge and the PR `input` names in the repository of `context`, or
    /// the space's linked PR.
    pub(super) async fn context_pull(
        &self,
        context: &ItemContext,
        input: Option<ItemInput>,
    ) -> Result<(ForgeRepo, ItemRef)> {
        match context {
            ItemContext::Checkout(selector) => {
                let checkout = self.repository(selector).await?;
                let input = input.context(super::actions::NOTHING_LINKED)?;
                let forge = self.checkout_forge(&checkout).await?;
                let pull = ItemRef::in_repository(&forge, ItemKind::Pr.into(), &input)?;
                Ok((forge, pull))
            }
            _ => {
                let space = self.context_space(context).await?;
                let forge = self.space_forge(&space).await?;
                let pull =
                    (self.named_or_linked(input, Some(space.id()), &forge, ItemKind::Pr)).await?;
                Ok((forge, pull))
            }
        }
    }

    /// The records of the workspaces that link the PR `url` which list
    /// files changed on both sides.
    async fn recorded_for_pull(&self, url: ItemRef) -> Result<Vec<ChangedOnBothSides>> {
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

/// Whether the workspace `id` links the PR `url`.
fn links_pull(db: &rusqlite::Connection, id: &SpaceId, url: &ItemRef) -> Result<bool> {
    let links = super::wait::linked_items(db, id, Some(ItemKind::Pr))?;
    Ok(links.contains(url))
}

/// What a merge brings together: a head and the branch it merges into.
struct Merged {
    head: CommitId,
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
