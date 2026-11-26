//! `shoal diff`.
mod elsewhere;

use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        context::Context,
        ui::{self, Acting, Fallback},
    },
    execution, git,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn diff(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let reading = reading();
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&reading));
    let base = (crate::client::diffs::diff_base(workspace.await?))
        .send(&ctx.paths)
        .await?;
    // Another machine has the worktree, so its Git writes the diff.
    if base.machine.is_some() {
        return elsewhere::diff(ctx, SpaceSelector::Id(base.workspace_id)).await;
    }
    let command = git::diff_command(base.commit.as_str());
    if crate::env::own_chat().is_some() {
        return diff_untracked(&base.path, &command);
    }
    shoal_workers::execution::run(
        &ctx.paths,
        SpaceSelector::Id(base.workspace_id),
        command,
        None,
    )
    .await
}

/// A chat reads a workspace without running anything there as the workspace's,
/// so no tracked execution counts the diff as its use.
fn diff_untracked(path: &Path, command: &[OsString]) -> Result<i32> {
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(path)
        .status()
        .context("run git diff")?;
    Ok(execution::exit_code(status))
}

/// A read of the base it diffs against, which another machine's workspace's
/// machine answers.
pub(super) fn reading() -> Acting {
    Acting::request(crate::client::diffs::diff_base)
}
