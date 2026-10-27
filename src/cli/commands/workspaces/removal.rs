//! What commands that remove workspaces share: leaving the removed
//! directory and refusing a batch member.
use std::path::PathBuf;

use anyhow::{Result, ensure};

use crate::{
    cli::{client, context::Context, ui},
    model::Workspace,
    removal::RemovalCheck,
    shell,
    state::WorkspaceState,
};

/// Refuse a workspace that a batch, named by its `(noun, verb)`, could not
/// finish with: one in another lifecycle operation, which the removal check
/// leaves to the removal itself, or one whose removal closes this terminal's
/// tab, ending the batch before the others.
pub(in crate::cli::commands) fn ensure_batch_removable(
    ctx: &Context,
    check: &RemovalCheck,
    (noun, verb): (&str, &str),
) -> Result<()> {
    let state = check.workspace.state;
    ensure!(
        matches!(state, WorkspaceState::Ready | WorkspaceState::Failed),
        "workspace is {state}; wait for that operation to finish"
    );
    ensure!(
        !crate::cli::terminal::closes_with(&ctx.paths, &check.workspace.id),
        "this terminal closes with the workspace, ending the {noun} of the others; {verb} from another terminal"
    );
    Ok(())
}

/// Run `removal`, which removes `workspace`, after finding where the shell
/// goes when its directory disappears under it, and send it there.
pub(in crate::cli::commands) async fn leaving<T>(
    ctx: &Context,
    workspace: &Workspace,
    removal: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let escape = escape_destination(ctx, workspace).await?;
    let result = removal.await;
    if let Some(destination) = escape
        && (result.is_ok() || !std::env::current_dir().is_ok_and(|cwd| cwd.exists()))
    {
        shell::navigate(&destination, ctx.json)?;
    }
    result
}

/// Where the shell should go if the current directory is inside `workspace`:
/// its repository's Shoal directory, or home when that is unavailable.
async fn escape_destination(ctx: &Context, workspace: &Workspace) -> Result<Option<PathBuf>> {
    let Some(cwd) = ui::current_directory()? else {
        return Ok(None);
    };
    if !workspace.contains(&cwd) {
        return Ok(None);
    }
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|r| r.id == workspace.repository_id)
        .and_then(|r| r.workspaces_dir)
        .filter(|p| p.is_dir());
    Ok(Some(repository.unwrap_or_else(|| ctx.paths.home.clone())))
}
