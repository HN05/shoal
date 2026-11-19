//! Deleting branches on the forge. Every named branch is checked first, and
//! none goes while one is refused: the default branch, a protected one, the
//! head or base of an open PR, one whose changes the default branch lacks
//! unless forced, and the branch of an active space outside the caller's
//! scope. A scoped caller deletes as its `push` permission allows, each
//! branch at the commit it was read at, which an approval binds.
use anyhow::{Result, bail};
use shoal_refs::BranchName;

use super::ReadBranches;
use crate::{
    branches::{DeletedBranch, ListedBranch, SpaceWork},
    config::permissions::PermissionKey,
    daemon::{links::place::Access, permissions::Action, scope::Caller, workspace::Manager},
    protocol::RepoTarget,
};
use shoal_forge::branches::BranchRequests;

impl Manager {
    /// Delete the branches `names` names on the forge of the `target`
    /// repository, those whose changes the default branch lacks only with
    /// `force`, or none when one is refused.
    pub async fn delete_branches(
        &self,
        target: &RepoTarget,
        names: &[BranchName],
        force: bool,
        caller: Option<&Caller>,
    ) -> Result<Vec<DeletedBranch>> {
        let read = self.read_branches(target, Access::Change, caller).await?;
        let doomed = self.deletable(&read, names, force, caller).await?;
        self.authorize_deletions(&read, &doomed, caller).await?;
        let mut deleted = Vec::new();
        for branch in doomed {
            let (name, head) = (&branch.branch.name, &branch.branch.head);
            read.forge.delete_branch(&read.account, name, head).await?;
            deleted.push(DeletedBranch {
                name: name.clone(),
                head: head.clone(),
            });
        }
        Ok(deleted)
    }

    /// The branches of `read` that `names` names, once none is refused.
    async fn deletable<'a>(
        &self,
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
            match self.refusal(read, branch, force, caller).await? {
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
        &self,
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
            && !self.reaches(caller, &space.id).await?
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

    /// Let a scoped `caller` delete `doomed` as its `push` permission
    /// allows: at once, once an approval of each at the commit it points to
    /// lets it, or not at all.
    async fn authorize_deletions(
        &self,
        read: &ReadBranches,
        doomed: &[&ListedBranch],
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let repository = read.checkout.as_ref().map(|checkout| checkout.id.clone());
        (self.refuse_denied(caller, PermissionKey::Push, repository.as_ref())).await?;
        let actions = (doomed.iter())
            .map(|branch| deletion(read, branch))
            .collect();
        self.authorize_actions(Some(caller), actions).await
    }
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
