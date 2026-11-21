//! Claude Code's `WorktreeCreate` and `WorktreeRemove` hooks. A worktree it
//! creates, as for a subagent with `isolation: "worktree"`, is a child
//! workspace of the workspace it runs in: the subagent works there as
//! the child, the child's activity reaches the agent, and Shoal's cleanup
//! removes it.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, de::DeserializeOwned};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    client::{
        assignments,
        connection::CommandPaths,
        output::{self, spaces::workspace_done},
        preparation::{self, NewChild},
    },
    model::Workspace,
};

/// What `WorktreeCreate` passes besides the common fields.
#[derive(Deserialize)]
struct Create {
    /// The worktree's slug, which names the child's branch.
    name: String,
    /// Where Claude Code works, inside the parent workspace.
    cwd: PathBuf,
}

/// What `WorktreeRemove` passes besides the common fields.
#[derive(Deserialize)]
struct Remove {
    /// The path `WorktreeCreate` returned.
    worktree_path: PathBuf,
}

/// Split a ready child from the workspace Claude Code works in and print its
/// path, the last line Claude Code reads as the worktree. Failing fails the
/// worktree's creation. `json` says whether setup's output stays off
/// stdout.
pub async fn create(paths: &CommandPaths, json: bool) -> Result<i32> {
    let input: Create = hook_input()?;
    let workspaces = crate::client::spaces::list(paths).await?.into_workspaces();
    let parent = super::enclosing(&workspaces, &input.cwd)?.id.clone();
    let start = NewChild::new(SpaceSelector::id(&parent), Some(input.name));
    let preparing = preparation::create(paths, start, crate::env::without_secrets()).await?;
    let child = crate::preparation::set_up(paths, json, preparing).await?;
    shoal_core::outln!("{}", child.path.display());
    Ok(0)
}

/// Mark the child at the worktree's path done, so Shoal removes it once its
/// work is pushed or merged and keeps unfinished work. Exiting 0 tells Claude
/// Code the worktree is gone, which is what it needs: Shoal owns the removal.
/// `json` says whether to print the completion as JSON.
pub async fn remove(paths: &CommandPaths, json: bool) -> Result<i32> {
    let input: Remove = hook_input()?;
    let workspaces = crate::client::spaces::list(paths).await?.into_workspaces();
    match rooted_at(&workspaces, &input.worktree_path) {
        // Its subagent may already have finished it.
        Some(child) if done_at_head(paths, child).await? => Ok(0),
        Some(child) => {
            let child = SpaceSelector::id(&child.id);
            let completion = assignments::mark_done(paths, child, Default::default()).await?;
            let message = workspace_done(&completion);
            shoal_core::outln!("{}", output::line(json, &message, &completion)?);
            Ok(0)
        }
        // Already removed, so nothing is left to clean up.
        None => Ok(0),
    }
}

/// Whether `child`'s assignment is already marked done at its current HEAD.
async fn done_at_head(paths: &CommandPaths, child: &Workspace) -> Result<bool> {
    let completion = crate::client::workspaces::inspect(paths, SpaceSelector::id(&child.id))
        .await?
        .completion;
    let Some(completion) = completion else {
        return Ok(false);
    };
    let (head, _) = crate::git::head_and_branch(&child.path).await?;
    Ok(completion.head == head)
}

/// The hook's JSON input on stdin.
fn hook_input<T: DeserializeOwned>() -> Result<T> {
    serde_json::from_reader(std::io::stdin().lock()).context("read the hook's input")
}

/// The workspace whose worktree is at `path`.
fn rooted_at<'a>(workspaces: &'a [Workspace], path: &Path) -> Option<&'a Workspace> {
    let path = super::canonical(path);
    workspaces.iter().find(|workspace| workspace.path == path)
}
