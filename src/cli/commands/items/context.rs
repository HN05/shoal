//! Where an item command that needs only a repository runs: a workspace, or
//! a registered checkout, where `shoal chat` runs its agent and where the
//! user names a registered repository to act in without a workspace.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{client, context::Context, ui},
    env,
    forge::{ForgeRepo, IssueInput},
    model::Repository,
    protocol::ItemContext,
    repository::{self, NotRegistered},
};

/// What is missing when nothing gives a non-interactive command its repository.
const NO_CONTEXT: &str =
    "no current workspace or registered checkout; run this in one, or pass --workspace or --repo";

/// The checkout of the registered repository `repo` names, else the context
/// [`select_context`] selects, except that outside a workspace or checkout
/// an issue or PR URL `item` of a registered repository selects that one's.
pub(in crate::cli::commands) async fn select_item_context(
    ctx: &Context,
    workspace: Option<String>,
    repo: Option<String>,
    item: Option<&str>,
) -> Result<ItemContext> {
    match repo {
        Some(repo) => Ok(ItemContext::Checkout(named(ctx, &repo).await?.id)),
        None => select_context(ctx, workspace, item).await,
    }
}

/// The context of a command whose `--repo` names any repository, and the
/// repository it then names there. The user's command runs in a registered
/// repository's checkout, without a workspace; any other repository is
/// reached through the context [`select_context`] selects.
pub(in crate::cli::commands) async fn select_repo_context(
    ctx: &Context,
    workspace: Option<String>,
    repo: Option<String>,
) -> Result<(ItemContext, Option<String>)> {
    if let (None, Some(named)) = (&workspace, &repo)
        && !env::inherits_scope(&ctx.paths.state)
        && let Some(checkout) = registered(ctx, named).await?
    {
        return Ok((ItemContext::Checkout(checkout.id), None));
    }
    // Refuse a bad repository before the workspace picker asks for anything.
    repo.as_deref().map(ForgeRepo::check_named).transpose()?;
    Ok((select_context(ctx, workspace, None).await?, repo))
}

/// The named or current workspace, else the registered checkout the current
/// directory is in, else the registered repository of the issue or PR URL
/// `item`, else a workspace the user picks.
async fn select_context(
    ctx: &Context,
    workspace: Option<String>,
    item: Option<&str>,
) -> Result<ItemContext> {
    if workspace.is_some() {
        let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory);
        return Ok(ItemContext::Workspace(workspace.await?));
    }
    if let Some(context) = current_context(ctx).await? {
        return Ok(context);
    }
    let repos = client::repositories(&ctx.paths).await?;
    if let Some(url) = item.filter(|item| IssueInput::parse(item) == IssueInput::Url)
        && let Some(repo) = super::super::issues::registered_for_item(&repos, url).await?
    {
        return Ok(ItemContext::Checkout(repo.id.clone()));
    }
    ensure!(ctx.interactive(), "{NO_CONTEXT}");
    let workspace = ui::select_workspace(ctx, None, ui::Fallback::CurrentDirectory).await?;
    Ok(ItemContext::Workspace(workspace))
}

/// The workspace, else the registered checkout, the current directory is in.
pub(in crate::cli::commands) async fn current_context(
    ctx: &Context,
) -> Result<Option<ItemContext>> {
    match ui::select_workspace(ctx, None, ui::Fallback::CurrentDirectoryOnly).await {
        Ok(workspace) => return Ok(Some(ItemContext::Workspace(workspace))),
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {}
        Err(error) => return Err(error),
    }
    let repos = client::repositories(&ctx.paths).await?;
    let checkout = super::super::issues::current_checkout(&repos).await?;
    Ok(checkout.map(|repo| ItemContext::Checkout(repo.id.clone())))
}

/// The registered repository `repo` names, a path read from the current
/// directory.
async fn named(ctx: &Context, repo: &str) -> Result<Repository> {
    let selector = ui::repository_selector(repo.to_owned())?;
    let repos = client::repositories(&ctx.paths).await?;
    Ok(repository::select(&repos, selector).await?.clone())
}

/// The registered repository `repo` names, if any.
async fn registered(ctx: &Context, repo: &str) -> Result<Option<Repository>> {
    match named(ctx, repo).await {
        Ok(repo) => Ok(Some(repo)),
        Err(error) if error.is::<NotRegistered>() => Ok(None),
        Err(error) => Err(error),
    }
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
