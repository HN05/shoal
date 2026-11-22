//! What commands that remove spaces share: leaving the removed directory
//! and refusing a batch member.
use std::path::PathBuf;

use anyhow::{Result, ensure};

use crate::{
    cli::{context::Context, shell, ui},
    model::Space,
    removal::SpaceRemovalCheck,
};

/// Refuse a space that a batch, named by its `(noun, verb)`, could not
/// finish with: one whose worktree is in another lifecycle operation, which
/// the removal check leaves to the removal itself, or one whose removal
/// closes this terminal's tab, ending the batch before the others. A
/// preparing workspace without commands is one whose setup never started,
/// which removal takes.
pub(in crate::cli::commands) async fn ensure_batch_removable(
    ctx: &Context,
    check: &SpaceRemovalCheck,
    (noun, verb): (&str, &str),
) -> Result<()> {
    check.ensure_settled()?;
    let space = &check.space;
    ensure!(
        !shoal_workers::hosting::closes_with(&ctx.paths, &space.id).await?,
        "this terminal closes with the {}, ending the {noun} of the others; {verb} from another terminal",
        space.kind()
    );
    Ok(())
}

/// Run `removal`, which removes `space`, after finding where the shell goes
/// when the directory of its worktree disappears under it, and send it
/// there.
pub(in crate::cli::commands) async fn leaving<T>(
    ctx: &Context,
    space: &Space,
    removal: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let escape = escape_destination(ctx, space).await?;
    let result = removal.await;
    if let Some(destination) = escape
        && (result.is_ok() || !std::env::current_dir().is_ok_and(|cwd| cwd.exists()))
    {
        shell::navigate(ctx, &destination)?;
    }
    result
}

/// Where the shell should go if the current directory is inside the
/// worktree of `space`: its repository's Shoal directory, or home when that
/// is unavailable.
pub(in crate::cli::commands) async fn escape_destination(
    ctx: &Context,
    space: &Space,
) -> Result<Option<PathBuf>> {
    let Some(workspace) = space.to_workspace() else {
        return Ok(None);
    };
    let Some(cwd) = ui::current_directory()? else {
        return Ok(None);
    };
    if !workspace.contains(&cwd) {
        return Ok(None);
    }
    let repository = crate::client::repositories::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|r| r.id == *workspace.repository())
        .map(|r| r.workspaces_dir)
        .filter(|p| p.is_dir());
    Ok(Some(repository.unwrap_or_else(|| ctx.paths.home.clone())))
}
