//! The branch a launch's new workspace starts on: the one it names as
//! typed, a branch link's, or the one earlier work on its issue left.
use anyhow::{Result, ensure};
use shoal_refs::BranchName;

use crate::{
    daemon::manager::Manager,
    forge::items::reference::ItemRef,
    git,
    launches::workspace,
    model::{Repository, Workspace},
    start_plan::Creation,
    state::WorkspaceState,
};

/// Open the pushed `branch` a branch link names.
pub(super) async fn branch_creation(branch: &str, creation: &mut Creation) -> Result<()> {
    git::check_branch_name(branch)?;
    creation.existing = Some(git::remote_ref("origin", branch));
    Ok(())
}

impl Manager {
    /// Settle the branch `creation` names as typed in `repo`, which
    /// `selector` names: the one of `workspaces` that already has it, which
    /// the start goes into instead, else the existing branch `creation`
    /// then opens, else the new branch it creates, which a name already
    /// taken would suffix and is refused.
    pub(super) async fn settle_branch(
        &self,
        workspaces: &[Workspace],
        (selector, repo): (&crate::repository::Selector, &Repository),
        creation: &mut Creation,
    ) -> Result<Option<Workspace>> {
        let Some(name) = creation.branch.take() else {
            return Ok(None);
        };
        let (remote, mut taken) = branches(repo, &name).await?;
        let workspaces = workspace::repository_workspaces(workspaces, repo);
        if let Some(workspace) = crate::launches::on_branch(&workspaces, &name, remote.as_ref()) {
            // A remote branch opens a local one only where it tracks it.
            if remote.is_some() {
                self.check_opens(selector, &name).await?;
            }
            return Ok(Some(workspace.clone()));
        }
        if remote.is_some() || taken.contains(&name.as_str().into()) {
            ensure!(
                creation.copy.is_none() && creation.files.is_none(),
                "a copy starts a new branch, and branch {name} exists; name another"
            );
            creation.existing = Some(name);
            return Ok(None);
        }
        taken.extend(
            workspaces
                .into_iter()
                .filter_map(|workspace| workspace.branch().cloned()),
        );
        ensure!(
            git::repo::allocate_branch(&name, &taken) == name,
            "branch {name} conflicts with an existing branch or workspace; name another"
        );
        creation.branch = Some(name);
        creation.typed = true;
        Ok(None)
    }
}

/// The remote and branch `name` names in `repo`, if it names a remote one,
/// and the branch names a new branch must not take.
async fn branches(
    repo: &Repository,
    name: &str,
) -> Result<(Option<(String, BranchName)>, Vec<BranchName>)> {
    let read = async {
        let remote = git::remote_branch(&repo.path, name).await?;
        anyhow::Ok((remote, git::branch::names_in_use(&repo.path).await?))
    };
    let error = match read.await {
        Ok(read) => return Ok(read),
        Err(error) => error,
    };
    let name = repo.display_name();
    // Removed meanwhile, as `shoal repo rm` takes its checkout.
    if !crate::fsutil::off_runtime::try_exists(&repo.path)
        .await
        .unwrap_or(true)
    {
        let message = format!("repository {name} ({}) was removed", repo.id);
        return Err(crate::refusal::Refusal::new(crate::refusal::Reason::NotFound, message).into());
    }
    // As while a removal deletes its files: named by its ID, which a
    // retry of either takes.
    Err(error.context(format!(
        "read the branches of repository {name} ({})",
        repo.id
    )))
}

/// `creation` on the branch the issue at `url` with `title` settles in
/// `repo` when nothing else names one: the branch or one of `workspaces`
/// earlier work on it left, else a new branch named after it.
pub(super) async fn issue_settled(
    workspaces: &[Workspace],
    repo: &Repository,
    creation: Creation,
    issue: Option<(&ItemRef, &str)>,
) -> Result<Creation> {
    match issue {
        Some((url, title)) if creation.unsettled() => {
            let branch = workspace::issue_branch(url.issue()?.get(), title);
            issue_creation(workspaces, repo, creation, (branch, url)).await
        }
        _ => Ok(creation),
    }
}

/// `creation` on the branch `name` of an issue at `issue_url`: the local
/// branch or one of `workspaces` earlier work on the issue left, which
/// reopens rather than suffixing a new branch and is refused here when it
/// cannot, else the new branch `name`.
async fn issue_creation(
    workspaces: &[Workspace],
    repo: &Repository,
    mut creation: Creation,
    (name, issue_url): (String, &ItemRef),
) -> Result<Creation> {
    let mut associated = None;
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.repository() == &repo.id)
    {
        if (workspace.links.issues.iter()).any(|issue| issue.url == *issue_url) {
            ensure!(
                associated.is_none(),
                "multiple workspaces are associated with this issue; pass an explicit branch"
            );
            ensure_reopens(workspace)?;
            associated = Some(
                workspace
                    .require_branch("reopening for an issue")?
                    .to_owned(),
            );
        }
    }
    if let Some(branch) = associated {
        creation.existing = Some(branch.into());
        return Ok(creation);
    }
    let local = BranchName::from(name.as_str());
    if let Some(workspace) = workspaces
        .iter()
        .find(|workspace| workspace.repository() == &repo.id && workspace.branch() == Some(&local))
    {
        ensure_reopens(workspace)?;
    } else if git::ref_exists(&repo.path, &git::local_ref(&local)).await? {
        git::existing_branch::ensure_not_checked_out(&repo.path, &local).await?;
    } else {
        creation.branch = Some(name);
        return Ok(creation);
    }
    creation.existing = Some(name);
    Ok(creation)
}

/// Refuse to reopen `workspace` unless it is ready.
fn ensure_reopens(workspace: &Workspace) -> Result<()> {
    ensure!(
        workspace.state == WorkspaceState::Ready,
        "workspace {} is {}; inspect or set it up before reopening",
        workspace.name,
        workspace.state
    );
    Ok(())
}

#[cfg(test)]
mod tests;
