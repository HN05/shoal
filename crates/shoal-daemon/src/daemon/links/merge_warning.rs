//! What merging a PR or a branch warns about: the sensitive files
//! that the last conflict check of the merged head found changed on both
//! sides. It reads that record and checks nothing again, so merging stays as
//! fast without one.
use anyhow::{Context, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_refs::CommitId;

use crate::{
    daemon::{
        links::place::Access, manager::Manager, scope::Caller, workspace::conflicts::recorded,
    },
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
    protocol::RepoTarget,
};
use shoal_forge::{RepoRequests, items::action::ActionRequests};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The warning for merging the PR `input` names in the `target`
    /// repository, or the space's linked PR, as the workspaces that link it
    /// last checked its head against its base branch.
    pub async fn merge_warning(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
    ) -> Result<Option<MergeWarning>> {
        let (forge, url) = self.context_pull(target, input, Access::Read, None).await?;
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
        target: &RepoTarget,
        input: Option<ItemInput>,
        action: Action,
        confirmed: &ConfirmedWarning,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        ensure!(action.merges(), "only a merge confirms a merge warning");
        let (_, url) = self.context_pull(target, input, Access::Read, None).await?;
        let current = self.merge_warning(target, Some(url.clone().into())).await?;
        ensure!(
            current == confirmed.warning,
            "the merge warning changed since you confirmed the merge; run shoal merge again"
        );
        self.item_action(target, ItemKind::Pr, Some(url.into()), action, caller)
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

    /// Bind a merge in `action` of the PR `input` names, or of `target`'s
    /// linked PR, for a scoped `caller`, to the commit the PR's head has now, unless it names one,
    /// and to the `[merge] method` it merges by, unless it names one, and
    /// return what it does as an approval of it binds it: a later commit,
    /// another method or another message needs another decision, and the
    /// forge refuses to merge another commit.
    pub(crate) async fn bind_merge(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        action: &mut Action,
        caller: &Caller,
    ) -> Result<String> {
        let (target, url) = self
            .target_item(target, input, ItemKind::Pr, Access::Change, Some(caller))
            .await?;
        self.configure_merge(target.own_place(), action).await?;
        if action.merges() && action.merge_head().is_none() {
            let forge = target.into_forge()?;
            let pull = forge.pull_request(&self.agent_account(), &url).await?;
            let head = pull.head_commit.context("the forge names no PR head")?;
            action.bind_merge_head(head);
        }
        let at = (action.merge_head()).map_or_else(String::new, |head| format!(" at {head}"));
        Ok(format!(
            "{} {url}{at}{}",
            action.name(),
            merge_terms(action)
        ))
    }

    /// The forge and the PR `input` names in the `target` repository, or
    /// the space's linked PR, refusing a scoped `caller` a change elsewhere
    /// than in its own repository.
    pub(super) async fn context_pull(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<(ForgeRepo, ItemRef)> {
        let (target, pull) =
            (self.target_item(target, input, ItemKind::Pr, access, caller)).await?;
        Ok((target.into_forge()?, pull))
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

/// The method and squash message `action` merges by, as an approval of it
/// names them, the whole message so another body needs another decision:
/// ` by squash as "Fix login\n\nWhy"`; nothing for the forge's default.
fn merge_terms(action: &Action) -> String {
    let (Action::Merge {
        method, message, ..
    }
    | Action::AutoMerge {
        method, message, ..
    }) = action
    else {
        return String::new();
    };
    let mut terms = method.map_or_else(String::new, |method| format!(" by {method}"));
    if let Some(message) = message {
        terms.push_str(&format!(" as {:?}", message.trim()));
    }
    terms
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
