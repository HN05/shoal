//! The review gate of a PR merge: with `[review] require_for_merge` holding
//! the caller, a finished review by the workspace that owns the PR's head
//! branch must cover the head the merge names, with every finding resolved.
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_refs::CommitId;

use super::{place::Access, target::ForgeTarget};
use crate::{
    daemon::{reviews::gate::Gate, scope::Caller, workspace::Manager},
    forge::{
        PullRequest,
        items::{action::Action, link::ItemKind, reference::ItemInput},
    },
    model::Workspace,
    protocol::RepoTarget,
};
use shoal_forge::RepoRequests;

impl Manager {
    /// Hold a merge in `action` of the PR `input` names, or of `target`'s
    /// linked PR, to its review gate, binding it to the head the review
    /// covers so the forge merges nothing else; other actions pass.
    pub(crate) async fn gate_pull_merge(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        action: &mut Action,
        caller: Option<&Caller>,
    ) -> Result<()> {
        if !action.merges() {
            return Ok(());
        }
        let head = action.merge_head().cloned();
        if let Some(head) = self
            .require_pull_review(target, input, head, caller)
            .await?
        {
            action.bind_merge_head(head);
        }
        Ok(())
    }

    /// Refuse merging the PR `input` names, or `target`'s linked PR, at
    /// `head`, else at the head it has now, where its repository's `[review]
    /// require_for_merge` holds `caller`, unless a finished review of the
    /// workspace that owns its head branch covers that head with every
    /// finding resolved. Returns the head checked, which the merge must name,
    /// or none where the gate does not hold `caller`.
    pub(crate) async fn require_pull_review(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        head: Option<CommitId>,
        caller: Option<&Caller>,
    ) -> Result<Option<CommitId>> {
        let pull = self.target_item(target, input, ItemKind::Pr, Access::Read, caller);
        let (target, url) = pull.await?;
        let Some(repository) = self.target_repository(&target).await? else {
            return Ok(None);
        };
        let settings = self.repository_settings(&repository).await?;
        if !Gate::Merge.holds(&settings, caller) {
            return Ok(None);
        }
        let account = self.target_account(&target, Access::Read, caller);
        let pull = target.forge()?.pull_request(&account, &url).await?;
        let head = match head {
            Some(head) => head,
            None => (pull.head_commit.clone()).context("the forge names no PR head")?,
        };
        let owner = self.pull_owner(&repository, &pull).await?;
        self.require_reviewed(&owner, Gate::Merge, &head).await?;
        Ok(Some(head))
    }

    /// The registered repository `target` acts in, if any; a checkout
    /// whose `origin` cannot be read is taken for another repository's, so
    /// it never keeps a merge elsewhere from going ahead.
    async fn target_repository(&self, target: &ForgeTarget) -> Result<Option<RepositoryId>> {
        if let Some(place) = target.own_place() {
            return Ok(Some(place.repository_id().clone()));
        }
        let forge = target.forge()?;
        for repository in self.repositories().await? {
            if self.origin_forge(&repository.path).await.ok().as_ref() == Some(forge) {
                return Ok(Some(repository.id));
            }
        }
        Ok(None)
    }

    /// The workspace of `repository` that owns `pull`'s head branch, whose
    /// reviews are the PR's; a fork's head branch is in no repository here.
    async fn pull_owner(&self, repository: &RepositoryId, pull: &PullRequest) -> Result<Workspace> {
        let owner = match pull.fork {
            true => None,
            false => self.branch_owner_in(repository, &pull.head).await?,
        };
        let owner = owner.with_context(|| {
            format!(
                "merging needs a review by the workspace that owns the branch of {}, and no workspace owns {}; merge it on the forge",
                pull.url, pull.head
            )
        })?;
        self.workspace_by_id(&owner).await
    }
}

#[cfg(test)]
mod tests;
