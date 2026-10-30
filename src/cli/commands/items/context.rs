//! Where an item command that needs only a repository runs: a workspace, or
//! a registered checkout, where `shoal chat` runs its agent.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{client, context::Context, ui},
    model::Repository,
    protocol::ItemContext,
};

/// What is missing when nothing gives a non-interactive command its repository.
const NO_CONTEXT: &str =
    "no current workspace or registered checkout; run this in one, or pass --workspace";

/// The named or current workspace, else the registered checkout the current
/// directory is in, else a workspace the user picks.
pub(in crate::cli::commands) async fn select_context(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<ItemContext> {
    if workspace.is_some() {
        let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory);
        return Ok(ItemContext::Workspace(workspace.await?));
    }
    match ui::select_workspace(ctx, None, ui::Fallback::CurrentDirectoryOnly).await {
        Ok(workspace) => return Ok(ItemContext::Workspace(workspace)),
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {}
        Err(error) => return Err(error),
    }
    let repos = client::repositories(&ctx.paths).await?;
    if let Some(repo) = super::super::issues::current_checkout(&repos).await? {
        return Ok(ItemContext::Checkout(repo.id.clone()));
    }
    ensure!(ctx.interactive(), "{NO_CONTEXT}");
    let workspace = ui::select_workspace(ctx, None, ui::Fallback::CurrentDirectory).await?;
    Ok(ItemContext::Workspace(workspace))
}

/// The repository registered with the checkout ID `repository`.
pub(in crate::cli::commands) async fn checkout_repository(
    ctx: &Context,
    repository: &str,
) -> Result<Repository> {
    client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == repository)
        .context("the checkout's repository is no longer registered")
}
