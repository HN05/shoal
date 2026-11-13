//! `shoal pr open`'s operation: the push of the workspace's branch, then
//! the PR the daemon finds or opens.
use anyhow::Result;

use super::{Failed, Reporter};
use crate::{
    client::{links::OpenOutcome, space_names::SpaceSelector},
    forge::items::create::PullOptions,
    git,
    model::Workspace,
    paths::CommandPaths,
    protocol::operations::{Failure, Step},
};

/// Verify the worktree before anything leaves it, push without forcing
/// unless the `push` permission holds the push back for the user, then let
/// the daemon find or open the PR and link it, or record what waits for the
/// user.
pub(super) async fn push_and_open(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    options: PullOptions,
    reporter: &Reporter<'_>,
) -> Result<OpenOutcome> {
    let workspace = crate::client::workspaces::verify(paths, workspace, true).await?;
    workspace.require_branch("shoal pr open")?;
    crate::client::reviews::check_pr_gate(paths, SpaceSelector::id(&workspace.id)).await?;
    let held = crate::client::approvals::push_held(paths, SpaceSelector::id(&workspace.id)).await?;
    if !held {
        reporter.step(Step::PushStarted).await?;
        push(paths, &workspace).await?;
        reporter.step(Step::Pushed).await?;
    }
    crate::client::items::open_pull(paths, SpaceSelector::id(&workspace.id), options, held).await
}

/// Push to the branch's push remote as a tracked command under the agent
/// account, with the user's Git configuration and hooks.
async fn push(paths: &CommandPaths, workspace: &Workspace) -> Result<()> {
    let branch = workspace.require_branch("shoal pr open")?;
    let remote = git::push_remote(&workspace.path, branch).await?;
    let code = crate::execution::publish(
        paths,
        SpaceSelector::Id(workspace.id.clone()),
        git::push_command(&remote, branch),
    )
    .await?;
    if code != 0 {
        let message = format!("git push of {branch} to {remote} failed (exit {code})");
        return Err(Failed(Failure::PushFailed { message }).into());
    }
    Ok(())
}
