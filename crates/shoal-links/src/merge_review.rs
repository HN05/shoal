//! The local review a PR merge waits for under `[review]
//! require_for_merge`, by the workspace that owns the PR's head.
use anyhow::{Context, Result};
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::forge_access::ForgeAccess;
use shoal_forge::{RepoRequests, capability::CapabilityRequests};
use shoal_forge_types::{
    PullRequest,
    capability::Capability,
    items::{action::Action, link::ItemKind, reference::ItemInput},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::{model::Workspace, protocol::RepoTarget};
use shoal_refs::CommitId;

use crate::{Links, daemon::Daemon, place::Access, target::ForgeTarget};

impl Links {
    /// Hold a merge in `action` of the PR `input` names, or of `target`'s
    /// linked PR, to its review gate, binding it to the head the review
    /// covers so the forge merges nothing else; other actions pass.
    pub async fn gate_pull_merge(
        &self,
        daemon: &dyn Daemon,
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
        let reviewed = self.require_pull_review(daemon, target, input, head, binding, caller);
        if let Some(head) = reviewed.await? {
            action.bind_merge_head(head);
        }
        Ok(())
    }

    /// Refuse merging the PR `input` names, or `target`'s linked PR, at
    /// `head` or else its current head, while `[review] require_for_merge`
    /// holds `caller` and no finished review of the PR's workspace covers
    /// that head with every finding resolved. An unregistered repository
    /// takes the setting from the global config. The forge must have
    /// `binding`, which keeps the merge at that head. Returns the head
    /// checked, which the merge must name, or none where the gate does not
    /// hold `caller`.
    pub async fn require_pull_review(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
        head: Option<CommitId>,
        binding: Option<Capability>,
        caller: Option<&Caller>,
    ) -> Result<Option<CommitId>> {
        let pull = self.target_item(daemon, target, input, ItemKind::Pr, Access::Read, caller);
        let (target, url) = pull.await?;
        let repository = target_repository(daemon, &self.forges, &target).await?;
        let settings = daemon.settings_in(repository.as_ref()).await?;
        if !daemon.merge_review_required(&settings, caller) {
            return Ok(None);
        }
        let forge = target.forge()?;
        if let Some(binding) = binding {
            forge.require(binding).context(UNBOUND)?;
        }
        let account = self.target_account(&target, Access::Read, caller);
        let pull = forge.pull_request(&account, &url).await?;
        let owner = pull_owner(daemon, repository.as_ref(), &pull).await?;
        let owner = owner.with_context(|| unreviewable(&pull, repository.is_some()))?;
        let head = match head {
            Some(head) => head,
            None => (pull.head_commit.clone()).context("the forge names no PR head")?,
        };
        (daemon.require_merge_review(&owner, &head, &url, caller)).await?;
        Ok(Some(head))
    }
}

/// The registered repository `target` acts in, if any; a checkout whose
/// `origin` cannot be read is taken for another repository's, so it never
/// keeps a merge elsewhere from going ahead.
async fn target_repository(
    daemon: &dyn Daemon,
    forges: &ForgeAccess,
    target: &ForgeTarget,
) -> Result<Option<RepositoryId>> {
    if let Some(place) = target.own_place() {
        return Ok(Some(place.repository_id().clone()));
    }
    let forge = target.forge()?;
    for repository in daemon.repositories().await? {
        if forges.origin_forge(&repository.path).await.ok().as_ref() == Some(forge) {
            return Ok(Some(repository.id));
        }
    }
    Ok(None)
}

/// The workspace of the registered `repository` whose reviews are `pull`'s,
/// the one `shoal review run` reviews it in: the one that owns its head
/// branch, or for a fork's, which is in no repository here, the one at a
/// commit made for or linked to it.
async fn pull_owner(
    daemon: &dyn Daemon,
    repository: Option<&RepositoryId>,
    pull: &PullRequest,
) -> Result<Option<Workspace>> {
    let Some(repository) = repository else {
        return Ok(None);
    };
    if pull.fork {
        return (daemon.pull_workspace(repository, &pull.url, pull.number)).await;
    }
    match daemon.branch_owner(repository, &pull.head).await? {
        Some(owner) => daemon.workspace_by_id(&owner).await.map(Some),
        None => Ok(None),
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
