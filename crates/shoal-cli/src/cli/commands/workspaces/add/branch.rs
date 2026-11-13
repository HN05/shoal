//! The branch a start names as typed, which opens as `git switch` takes it:
//! an existing local or remote branch opens, in the workspace that already
//! has it, and any other name is a new branch under exactly that name.
use anyhow::{Context as _, Result, ensure};
use shoal_refs::BranchName;

use super::target::AddTarget;
use crate::{
    cli::context::Context,
    git,
    model::{Repository, Workspace},
    start_plan::Creation,
};

/// Settle the branch `creation` names as typed: the workspace that already
/// has it, which the start goes into instead, else the existing branch
/// `creation` then opens, else the new branch it creates, which a name
/// already taken would suffix and is refused.
pub(super) async fn settle(
    ctx: &Context,
    target: &AddTarget,
    creation: &mut Creation,
) -> Result<Option<Workspace>> {
    let Some(name) = creation.branch.take() else {
        return Ok(None);
    };
    let repo = target.repository(ctx).await?;
    let (remote, mut taken) = branches(repo, &name).await.with_context(|| {
        format!(
            "read the branches of repository {}",
            crate::repository::name(repo)
        )
    })?;
    let workspaces = repository_workspaces(ctx, repo).await?;
    if let Some(workspace) = on_branch(&workspaces, &name, remote.as_ref()) {
        // A remote branch opens a local one only where it tracks it.
        if remote.is_some() {
            check_opens(ctx, target, &name).await?;
        }
        return Ok(Some(workspace.clone()));
    }
    if remote.is_some() || taken.contains(&name.as_str().into()) {
        ensure!(
            creation.copy.is_none(),
            "a copy starts a new branch, and branch {name} exists; name another"
        );
        creation.existing = Some(name);
        return Ok(None);
    }
    taken.extend(
        workspaces
            .into_iter()
            .filter_map(|workspace| workspace.branch),
    );
    ensure!(
        git::repo::allocate_branch(&name, &taken) == name,
        "branch {name} conflicts with an existing branch or workspace; name another"
    );
    creation.branch = Some(name);
    creation.typed = true;
    Ok(None)
}

/// The workspace of `repo` that already has the branch `name` selects, as
/// typed or as a remote's, which a start on it goes into.
pub(in crate::cli::commands) async fn owning_workspace(
    ctx: &Context,
    repo: &Repository,
    name: &str,
) -> Result<Option<Workspace>> {
    let remote = git::remote_branch(&repo.path, name).await?;
    let workspaces = repository_workspaces(ctx, repo).await?;
    Ok(on_branch(&workspaces, name, remote.as_ref()).cloned())
}

/// The workspaces of `repo`.
async fn repository_workspaces(ctx: &Context, repo: &Repository) -> Result<Vec<Workspace>> {
    Ok((crate::client::workspaces::list(&ctx.paths).await?)
        .into_iter()
        .filter(|workspace| workspace.repository_id == repo.id)
        .collect())
}

/// The workspace among `workspaces` on the local branch `name` selects: its
/// own name, or the branch of the `remote` one it names.
fn on_branch<'a>(
    workspaces: &'a [Workspace],
    name: &str,
    remote: Option<&(String, BranchName)>,
) -> Option<&'a Workspace> {
    let local = remote.map_or(name, |(_, branch)| branch.as_str());
    (workspaces.iter())
        .find(|workspace| workspace.branch.as_ref().map(BranchName::as_str) == Some(local))
}

/// The remote and branch `name` names in `repo`, if it names a remote one,
/// and the branch names a new branch must not take.
async fn branches(
    repo: &Repository,
    name: &str,
) -> Result<(Option<(String, BranchName)>, Vec<BranchName>)> {
    let remote = git::remote_branch(&repo.path, name).await?;
    Ok((remote, git::branch::names_in_use(&repo.path).await?))
}

/// Refuse what opening the existing branch `selector` would refuse, such as
/// a local branch of its name that does not track it.
async fn check_opens(ctx: &Context, target: &AddTarget, selector: &str) -> Result<()> {
    let planned = crate::client::workspaces::PlannedWorkspace {
        repository: target.selector.clone(),
        branch: None,
        existing: Some(selector.to_owned()),
        base: None,
        git_profile: None,
        path: None,
        parent: None,
        options: Default::default(),
    };
    crate::client::workspaces::check_creation(&ctx.paths, planned).await
}
