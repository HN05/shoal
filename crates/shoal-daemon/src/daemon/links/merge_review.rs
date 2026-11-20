//! The review gate of a PR merge: with `[review] require_for_merge` holding
//! the caller, a finished review by the PR's workspace, the one that owns
//! its head branch or, for a fork's, the one at a commit made for or linked
//! to it, must cover the head the merge names, with every finding resolved.
use anyhow::{Context, Result, bail};
use shoal_ids::repository_ids::RepositoryId;
use shoal_links::place::Access;
use shoal_links::target::ForgeTarget;
use shoal_refs::CommitId;
use shoal_reviews::required::{Gate, Unreviewed};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::{
        PullRequest,
        capability::Capability,
        items::{
            action::Action,
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::Workspace,
    protocol::RepoTarget,
};
use shoal_forge::{RepoRequests, capability::CapabilityRequests};
use shoal_links::pr::current_head;

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
        let binding = Some(binding(action));
        let reviewed = self.require_pull_review(target, input, head, binding, caller);
        if let Some(head) = reviewed.await? {
            action.bind_merge_head(head);
        }
        Ok(())
    }

    /// Refuse merging the PR `input` names, or `target`'s linked PR, at
    /// `head`, else at the head it has now, where `[review]
    /// require_for_merge` of its repository, or of the global config for an
    /// unregistered one, holds `caller`, unless a finished review of the
    /// PR's workspace covers that head with every finding resolved, and the
    /// forge has `binding`, which keeps the merge at that head. Returns the
    /// head checked, which the merge must name, or none where the gate does
    /// not hold `caller`.
    pub(crate) async fn require_pull_review(
        &self,
        target: &RepoTarget,
        input: Option<ItemInput>,
        head: Option<CommitId>,
        binding: Option<Capability>,
        caller: Option<&Caller>,
    ) -> Result<Option<CommitId>> {
        let pull = self
            .links
            .target_item(self, target, input, ItemKind::Pr, Access::Read, caller);
        let (target, url) = pull.await?;
        let repository = self.target_repository(&target).await?;
        let settings = self.space_settings_in(repository.as_ref()).await?;
        if !Gate::Merge.holds(&settings, caller) {
            return Ok(None);
        }
        let forge = target.forge()?;
        if let Some(binding) = binding {
            forge.require(binding).context(UNBOUND)?;
        }
        let account = self.links.target_account(&target, Access::Read, caller);
        let pull = forge.pull_request(&account, &url).await?;
        let owner = self.pull_owner(repository.as_ref(), &pull).await?;
        let owner = owner.with_context(|| unreviewable(&pull, repository.is_some()))?;
        let head = match head {
            Some(head) => head,
            None => (pull.head_commit.clone()).context("the forge names no PR head")?,
        };
        let checked = self
            .reviews
            .check_reviewed(&owner, Gate::Merge, &head)
            .await;
        self.queue_pull_review(checked, &owner, &head, &url, caller)
            .await?;
        Ok(Some(head))
    }

    /// `checked`, the gate of `owner` at `head`, with a review queued for an
    /// [`Unreviewed`] refusal where that review covers `head`, as a review
    /// of the owner's HEAD does when it is `head`, and `caller`'s scope
    /// reaches the owner; otherwise the refusal names how to review the PR.
    async fn queue_pull_review(
        &self,
        checked: Result<()>,
        owner: &Workspace,
        head: &CommitId,
        url: &ItemRef,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Err(error) = checked else {
            return Ok(());
        };
        if error.downcast_ref::<Unreviewed>().is_none() {
            return Err(error);
        }
        let at_head = current_head(owner).await.ok().as_ref() == Some(head);
        let reached = match caller {
            Some(caller) => self.reaches(caller, &owner.id).await?,
            None => true,
        };
        match at_head && reached {
            true => self.reviews.queue_unreviewed(self, Err(error)).await,
            false => bail!("{error}; shoal review run {url} reviews the PR"),
        }
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
            if shoal_links::forges::origin(self, &repository.path)
                .await
                .ok()
                .as_ref()
                == Some(forge)
            {
                return Ok(Some(repository.id));
            }
        }
        Ok(None)
    }

    /// The workspace of the registered `repository` whose reviews are
    /// `pull`'s, the one `shoal review run` reviews it in: the one that
    /// owns its head branch, or for a fork's, which is in no repository
    /// here, the one at a commit made for or linked to it.
    async fn pull_owner(
        &self,
        repository: Option<&RepositoryId>,
        pull: &PullRequest,
    ) -> Result<Option<Workspace>> {
        let Some(repository) = repository else {
            return Ok(None);
        };
        if pull.fork {
            return (self.pull_workspace(repository, &pull.url, pull.number)).await;
        }
        match self.branch_owner_in(repository, &pull.head).await? {
            Some(owner) => self.workspace_by_id(&owner).await.map(Some),
            None => Ok(None),
        }
    }
}

/// Why a merge the gate holds needs the forge to keep it at its head.
const UNBOUND: &str = "[review] require_for_merge merges only the reviewed head, which this forge cannot hold the merge to";

/// The refusal of a merge of `pull`, which has no workspace, so no review
/// covers it; only a PR of a `registered` repository can have one.
fn unreviewable(pull: &PullRequest, registered: bool) -> String {
    let url = &pull.url;
    let missing = match (registered, pull.fork) {
        (false, _) => {
            return format!(
                "merging {url} needs a review of its head, and its repository is not registered here; merge it on the forge"
            );
        }
        (true, true) => "no workspace is at a commit of it".to_owned(),
        (true, false) => format!("no workspace owns {}", pull.head),
    };
    format!(
        "merging {url} needs a review of its head, and {missing}; shoal review run {url} reviews the PR"
    )
}

/// The capability that keeps `action`, a merge, at the head it names.
fn binding(action: &Action) -> Capability {
    match action {
        Action::AutoMerge { .. } => Capability::AutoMergeAtHead,
        _ => Capability::MergeAtHead,
    }
}

#[cfg(test)]
mod tests;
