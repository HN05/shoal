//! The branches on the forge of a space's repository, or another one, with
//! what decides whether each can go: whether the default branch has its
//! changes, the open PRs it is the head of and the space whose branch it
//! is. A scoped caller reads them as the agent account, the user with their
//! own logins.
use std::path::Path;

use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};
use shoal_refs::{BranchName, CommitId};

use super::{place::Access, target::ForgeTarget};
use crate::{
    branches::{BranchSpace, ListedBranch, SpaceWork},
    daemon::{scope::Caller, workspace::Manager},
    forge::branches::{ForgeBranch, OpenPull},
    git,
    model::{Repository, Workspace},
    protocol::RepoTarget,
};
use shoal_forge::{branches::BranchRequests, ci::workflow::WorkflowRequests};

/// How many branches the merged check reads at once.
const CHECKS_AT_ONCE: usize = 8;

/// What describing a branch reads besides the branch.
struct Context<'a> {
    default: &'a BranchName,
    pulls: &'a [OpenPull],
    checkout: Option<&'a Repository>,
    workspaces: &'a [Workspace],
}

impl Manager {
    /// The branches on the forge of the `target` repository, only those
    /// whose merged state is known to be `merged` when it is given.
    pub async fn list_branches(
        &self,
        target: &RepoTarget,
        merged: Option<bool>,
        caller: Option<&Caller>,
    ) -> Result<Vec<ListedBranch>> {
        let listed = self.read_branches(target, Access::Read, caller).await?;
        let wanted = |branch: &ListedBranch| merged.is_none() || branch.merged == merged;
        Ok(listed.into_iter().filter(wanted).collect())
    }

    /// The branches on the forge of the `target` repository, read for
    /// `access` as [`Self::read_account`] chooses.
    async fn read_branches(
        &self,
        target: &RepoTarget,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<Vec<ListedBranch>> {
        let target = self.forge_target(target, None, access, caller).await?;
        let checkout = self.target_checkout(&target).await?;
        let (forge, account) = (target.into_forge()?, self.read_account(caller));
        let (branches, pulls, default) = tokio::try_join!(
            forge.branches(&account),
            forge.open_pulls(&account),
            forge.default_branch(&account),
        )?;
        let workspaces = match &checkout {
            Some(checkout) => self.repository_workspaces(&checkout.id).await?,
            None => Vec::new(),
        };
        let context = Context {
            default: &BranchName::from(default),
            pulls: &pulls,
            checkout: checkout.as_ref(),
            workspaces: &workspaces,
        };
        stream::iter(branches)
            .map(|branch| describe(branch, &context))
            .buffered(CHECKS_AT_ONCE)
            .try_collect()
            .await
    }

    /// The registered repository a request acts in as its own; none for
    /// another one.
    async fn target_checkout(&self, target: &ForgeTarget) -> Result<Option<Repository>> {
        match target.own_place() {
            Some(place) => Ok(Some(self.repository(place.repository_id()).await?)),
            None => Ok(None),
        }
    }
}

/// `branch` with what decides whether it can go.
async fn describe(branch: ForgeBranch, context: &Context<'_>) -> Result<ListedBranch> {
    let merged = match context.checkout {
        Some(checkout) => default_has(&checkout.path, &branch.head, context.default).await?,
        None => None,
    };
    let open_prs = (context.pulls.iter())
        .filter(|pull| !pull.fork && pull.head == branch.name)
        .map(|pull| pull.number)
        .collect();
    let space = (context.workspaces.iter())
        .find(|workspace| workspace.part.branch() == Some(&branch.name))
        .map(|workspace| BranchSpace {
            id: workspace.id.clone(),
            number: workspace.number,
            name: workspace.name.clone(),
            state: work(workspace.part.done, merged),
        });
    Ok(ListedBranch {
        default: &branch.name == context.default,
        branch,
        merged,
        open_prs,
        space,
    })
}

/// Whether the local `default` branch of the checkout at `path` has the
/// changes of `head`, as cleanup checks it, through squash and rebase
/// merges; none while the checkout has not fetched `head` or has no such
/// branch.
async fn default_has(path: &Path, head: &CommitId, default: &BranchName) -> Result<Option<bool>> {
    let default = git::local_ref(default);
    if !git::ref_exists(path, &default).await? || !git::has_commit(path, head.as_str()).await? {
        return Ok(None);
    }
    let merged = git::status::merged_into(path, head.as_str(), &default).await?;
    Ok(Some(merged))
}

/// Where a workspace's work stands: marked `done`, else merged where the
/// default branch is known to have its branch's changes, else active.
fn work(done: bool, merged: Option<bool>) -> SpaceWork {
    match (done, merged) {
        (true, _) => SpaceWork::Done,
        (false, Some(true)) => SpaceWork::Merged,
        (false, _) => SpaceWork::Active,
    }
}
