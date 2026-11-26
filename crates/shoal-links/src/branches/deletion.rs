//! Deleting branches on the forge, each only at the commit it was read at.
//! `docs/design/forge.md` says which branches go.
use anyhow::{Context, Result, bail};
use shoal_core::git;
use shoal_daemon_base::{callers::Caller, permitted_actions::Action};
use shoal_forge::branches::BranchRequests;
use shoal_protocol::{
    branches::{DeletedBranch, ListedBranch, SpaceWork},
    config::permissions::PermissionKey,
    protocol::RepoTarget,
};
use shoal_refs::BranchName;

use super::ReadBranches;
use crate::{Links, daemon::Daemon, place::Access};

impl Links {
    /// Delete the branches `names` names on the forge of the `target`
    /// repository, those whose changes the default branch lacks only with
    /// `force`, or none when one is refused.
    pub async fn delete_branches(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        names: &[BranchName],
        force: bool,
        caller: Option<&Caller>,
    ) -> Result<Vec<DeletedBranch>> {
        let read = (self.read_branches(daemon, target, Access::Change, caller)).await?;
        let doomed = deletable(daemon, &read, names, force, caller).await?;
        ensure_unmoved(&read, &doomed).await?;
        authorize_deletions(daemon, &read, &doomed, caller).await?;
        delete_checked(&read, &doomed).await
    }
}

/// The branches of `read` that `names` names, once none is refused.
async fn deletable<'a>(
    daemon: &dyn Daemon,
    read: &'a ReadBranches,
    names: &[BranchName],
    force: bool,
    caller: Option<&Caller>,
) -> Result<Vec<&'a ListedBranch>> {
    let mut doomed: Vec<&ListedBranch> = Vec::new();
    let mut refusals = Vec::new();
    for name in names {
        if doomed.iter().any(|branch| &branch.branch.name == name) {
            continue;
        }
        let Some(branch) = read
            .listed
            .iter()
            .find(|branch| &branch.branch.name == name)
        else {
            refusals.push(format!("{name}: no such branch on the forge"));
            continue;
        };
        match refusal(daemon, read, branch, force, caller).await? {
            Some(refusal) => refusals.push(format!("{name}: {refusal}")),
            None => doomed.push(branch),
        }
    }
    if !refusals.is_empty() {
        bail!(
            "deleted no branch; shoal branch ls shows what keeps each:\n  {}",
            refusals.join("\n  ")
        );
    }
    Ok(doomed)
}

/// Why `branch` cannot go, if it cannot.
async fn refusal(
    daemon: &dyn Daemon,
    read: &ReadBranches,
    branch: &ListedBranch,
    force: bool,
    caller: Option<&Caller>,
) -> Result<Option<String>> {
    let name = &branch.branch.name;
    if branch.default {
        return Ok(Some("it is the default branch".into()));
    }
    if branch.branch.protected {
        return Ok(Some("the forge protects it".into()));
    }
    if let Some(pull) = read.pulls.iter().find(|pull| pull.joins(name)) {
        let number = pull.number;
        return Ok(Some(format!(
            "it is the head or base of open PR #{number}; merge or close the PR first"
        )));
    }
    if let (Some(space), Some(caller)) = (&branch.space, caller)
        && space.state == SpaceWork::Active
        && !daemon.reaches(caller, &space.id).await?
    {
        let (number, space) = (space.number, &space.name);
        return Ok(Some(format!(
            "it is the branch of space {number} {space}, whose work goes on outside your \
                 scope; ask the user to delete it"
        )));
    }
    Ok(match (branch.merged, force) {
        (Some(true), _) | (_, true) => None,
        (Some(false), false) => {
            Some("the default branch lacks its changes; --force deletes it anyway".into())
        }
        (None, false) => Some(
            "Shoal cannot tell whether the default branch has its changes; --force deletes \
                 it anyway"
                .into(),
        ),
    })
}

/// Let a scoped `caller` delete `doomed` as its `push` permission allows:
/// at once, once an approval of each at the commit it points to lets it, or
/// not at all.
async fn authorize_deletions(
    daemon: &dyn Daemon,
    read: &ReadBranches,
    doomed: &[&ListedBranch],
    caller: Option<&Caller>,
) -> Result<()> {
    let Some(caller) = caller else {
        return Ok(());
    };
    let repository = read.checkout.as_ref().map(|checkout| checkout.id.clone());
    (daemon.refuse_denied(caller, PermissionKey::Push, repository.as_ref())).await?;
    let actions = (doomed.iter())
        .map(|branch| deletion(read, branch))
        .collect();
    daemon.authorize_actions(Some(caller), actions).await
}

/// Refuse every one of `doomed` before any is deleted, or any approval used,
/// once one of them no longer points to the commit `read` read it at.
async fn ensure_unmoved(read: &ReadBranches, doomed: &[&ListedBranch]) -> Result<()> {
    for branch in doomed {
        let (name, head) = (&branch.branch.name, &branch.branch.head);
        let unmoved = read.forge.ensure_branch_at(&read.account, name, head).await;
        unmoved.context("deleted no branch")?;
    }
    Ok(())
}

/// Delete each of `doomed` in turn, as `read`'s account, only at the
/// commit `read` read it at, stopping at the first that fails.
async fn delete_checked(
    read: &ReadBranches,
    doomed: &[&ListedBranch],
) -> Result<Vec<DeletedBranch>> {
    let login = read.account.git_login(&read.forge).await?;
    let url = read.forge.git_url();
    let mut deleted = Vec::new();
    for branch in doomed {
        let (name, head) = (&branch.branch.name, &branch.branch.head);
        git::remote_deletion::delete_at(&url, name, head, &login).await?;
        deleted.push(DeletedBranch {
            name: name.clone(),
            head: head.clone(),
        });
    }
    Ok(deleted)
}

/// Deleting `branch` of `read` at the commit it points to, as the `push`
/// permission governs it.
fn deletion(read: &ReadBranches, branch: &ListedBranch) -> Action {
    let (name, head) = (&branch.branch.name, &branch.branch.head);
    let path = &read.forge.path;
    let action = Action::new(
        PermissionKey::Push,
        format!("delete branch {name} at {head} in {path}"),
    );
    match &read.checkout {
        Some(checkout) => action.in_repository(checkout.id.clone()),
        None => action,
    }
}
