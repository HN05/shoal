//! Claude Code's `WorktreeCreate` and `WorktreeRemove` hooks. A worktree it
//! creates, as for a subagent with `isolation: "worktree"`, is a child
//! workspace split from the workspace it runs in: the subagent works there as
//! the child, the child's activity reaches the agent, and Shoal's cleanup
//! removes it.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, de::DeserializeOwned};

use crate::{
    cli::{client, context::Context},
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
/// worktree's creation.
pub(super) async fn create(ctx: &Context) -> Result<i32> {
    let input: Create = hook_input()?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let parent = enclosing(&workspaces, &input.cwd)?.id.clone();
    let child = super::split::create_child(ctx, parent, input.name, None, None).await?;
    let Some(child) = super::split::prepare(ctx, child).await? else {
        return Ok(1);
    };
    outln!("{}", child.path.display());
    Ok(0)
}

/// Mark the child at the worktree's path done, so Shoal removes it once its
/// work is pushed or merged and keeps unfinished work. Exiting 0 tells Claude
/// Code the worktree is gone, which is what it needs: Shoal owns the removal.
pub(super) async fn remove(ctx: &Context) -> Result<i32> {
    let input: Remove = hook_input()?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    match rooted_at(&workspaces, &input.worktree_path) {
        // Its subagent may already have finished it.
        Some(child) if done_at_head(ctx, child).await? => Ok(0),
        Some(child) => super::workspaces::done(ctx, Some(child.id.clone()), false).await,
        // Already removed, so nothing is left to clean up.
        None => Ok(0),
    }
}

/// Whether `child`'s assignment is already marked done at its current HEAD.
async fn done_at_head(ctx: &Context, child: &Workspace) -> Result<bool> {
    let completion = client::inspect(&ctx.paths, child.id.clone())
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
    let path = canonical(path);
    workspaces.iter().find(|workspace| workspace.path == path)
}

/// The innermost workspace containing `directory`, which a scoped caller
/// lists among its own and those below it.
pub(super) fn enclosing<'a>(
    workspaces: &'a [Workspace],
    directory: &Path,
) -> Result<&'a Workspace> {
    Workspace::innermost(workspaces, &canonical(directory))
        .with_context(|| format!("{} is in no Shoal workspace", directory.display()))
}

/// `path` with symlinks resolved, as workspace records hold it, or as given
/// when it does not exist.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
