//! The branches on the forge of a space's repository, or another one, with
//! what decides whether each can go: whether the default branch has its
//! changes, the open PRs it is the head of and the space whose branch it
//! is. A scoped caller reads them as the agent account, the user with their
//! own logins.
mod deletion;

use std::path::Path;

use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};
use shoal_core::git;
use shoal_daemon_base::callers::Caller;
use shoal_forge::{
    branches::BranchRequests, repo::default_branch::DefaultBranchRequests,
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    branches::{ForgeBranch, OpenPull},
};
use shoal_protocol::{
    branches::{BranchSpace, ListedBranch, SpaceWork},
    model::{Repository, Workspace},
    protocol::RepoTarget,
};
use shoal_refs::{BranchName, CommitId};

use crate::{Links, daemon::Daemon, place::Access, target::ForgeTarget};

/// How many branches the merged check reads at once.
const CHECKS_AT_ONCE: usize = 8;

/// A repository's branches on its forge as the daemon read them, with what
/// a deletion needs beyond them.
pub struct ReadBranches {
    pub forge: ForgeRepo,
    pub account: Account,
    /// The registered repository they are the branches of; none for one
    /// that is not registered.
    pub checkout: Option<Repository>,
    pub listed: Vec<ListedBranch>,
    pub pulls: Vec<OpenPull>,
}

/// What describing a branch reads besides the branch.
struct Context<'a> {
    default: &'a BranchName,
    pulls: &'a [OpenPull],
    checkout: Option<&'a Repository>,
    workspaces: &'a [Workspace],
}

impl Links {
    /// The branches on the forge of the `target` repository, only those
    /// whose merged state is known to be `merged` when it is given.
    pub async fn list_branches(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        merged: Option<bool>,
        caller: Option<&Caller>,
    ) -> Result<Vec<ListedBranch>> {
        let read = (self.read_branches(daemon, target, Access::Read, caller)).await?;
        let wanted = |branch: &ListedBranch| merged.is_none() || branch.merged == merged;
        Ok(read.listed.into_iter().filter(wanted).collect())
    }

    /// The branches on the forge of the `target` repository, read for
    /// `access` as [`Self::read_account`] chooses.
    pub async fn read_branches(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<ReadBranches> {
        let target = (self.forge_target(daemon, target, None, access, caller)).await?;
        let checkout = target_checkout(daemon, &target).await?;
        let (forge, account) = (target.into_forge()?, self.read_account(caller));
        let (branches, pulls, default) = tokio::try_join!(
            forge.branches(&account),
            forge.open_pulls(&account),
            forge.default_branch(&account),
        )?;
        let workspaces = checkout_workspaces(daemon, checkout.as_ref()).await?;
        let context = Context {
            default: &BranchName::from(default),
            pulls: &pulls,
            checkout: checkout.as_ref(),
            workspaces: &workspaces,
        };
        let listed = context.describe_all(branches).await?;
        Ok(ReadBranches {
            forge,
            account,
            checkout,
            listed,
            pulls,
        })
    }
}

/// The workspaces of `checkout`'s repository; none outside a registered one.
async fn checkout_workspaces(
    daemon: &dyn Daemon,
    checkout: Option<&Repository>,
) -> Result<Vec<Workspace>> {
    match checkout {
        Some(checkout) => daemon.repository_workspaces(&checkout.id).await,
        None => Ok(Vec::new()),
    }
}

/// The registered repository a request acts in as its own; none for
/// another one.
async fn target_checkout(daemon: &dyn Daemon, target: &ForgeTarget) -> Result<Option<Repository>> {
    match target.own_place() {
        Some(place) => Ok(Some(daemon.repository(place.repository_id()).await?)),
        None => Ok(None),
    }
}

impl Context<'_> {
    /// Each of `branches` with what decides whether it can go, a few read
    /// at once.
    async fn describe_all(&self, branches: Vec<ForgeBranch>) -> Result<Vec<ListedBranch>> {
        stream::iter(branches)
            .map(|branch| describe(branch, self))
            .buffered(CHECKS_AT_ONCE)
            .try_collect()
            .await
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
    let owner =
        (context.workspaces.iter()).find(|workspace| workspace.part.branch() == Some(&branch.name));
    let space = match owner {
        Some(workspace) => Some(branch_space(workspace, &branch.name, context).await?),
        None => None,
    };
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

/// `workspace`, whose branch `branch` is, with where its work stands:
/// marked `done`, else merged where the default branch is known to have the
/// changes of its local branch, which may hold commits the forge's lacks,
/// else active.
async fn branch_space(
    workspace: &Workspace,
    branch: &BranchName,
    context: &Context<'_>,
) -> Result<BranchSpace> {
    let state = if workspace.part.done {
        SpaceWork::Done
    } else if local_merged(branch, context).await? == Some(true) {
        SpaceWork::Merged
    } else {
        SpaceWork::Active
    };
    Ok(BranchSpace {
        id: workspace.id.clone(),
        number: workspace.number,
        name: workspace.name.clone(),
        state,
    })
}

/// Whether the default branch has the changes of the checkout's local
/// `branch`; none where Shoal cannot tell.
async fn local_merged(branch: &BranchName, context: &Context<'_>) -> Result<Option<bool>> {
    let Some(checkout) = context.checkout else {
        return Ok(None);
    };
    match git::status::local_branch_commit(&checkout.path, branch).await? {
        Some(local) => default_has(&checkout.path, &local, context.default).await,
        None => Ok(None),
    }
}
