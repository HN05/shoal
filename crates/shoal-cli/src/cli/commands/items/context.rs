//! Where an item command that needs only a repository runs: a workspace, or
//! a registered checkout, where a chat runs its agent and where the
//! user names a registered repository to act in without a workspace.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{context::Context, ui},
    env,
    forge::{ForgeRepo, items::reference::ItemInput},
    model::Repository,
    protocol::{ItemContext, RepoTarget},
    repository::{self, NotRegistered},
};
use shoal_protocol::space_names::SpaceSelector;

/// What is missing when nothing gives a non-interactive command its space
/// or repository.
const NO_CONTEXT: &str =
    "no current workspace, chat or registered checkout; run this in one, or pass --space or --repo";

/// What is missing when nothing gives a non-interactive command that takes
/// only `--repo` its repository.
const NO_REPOSITORY: &str =
    "no current workspace or registered checkout; run this in one, or pass --repo";

/// Where a command that acts on a space, a workspace or chat, runs: as
/// [`known_item_context`] finds it, else a space the user picks.
pub(in crate::cli::commands) async fn select_space_item_context(
    ctx: &Context,
    space: Option<SpaceSelector>,
    item: Option<&ItemInput>,
) -> Result<ItemContext> {
    match known_item_context(ctx, space, item).await? {
        Some(context) => Ok(context),
        None => picked_space(ctx).await,
    }
}

/// The repository a command about an issue, PR or CI run acts in: the one
/// `repo` names, as [`select_repo_context`] finds it, else that of the
/// context [`known_item_context`] finds, else the one the URL `item` names
/// from nowhere, else that of a space the user picks.
pub(in crate::cli::commands) async fn select_item_target(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    item: Option<&ItemInput>,
) -> Result<RepoTarget> {
    if repo.is_some() {
        return select_repo_context(ctx, repo).await;
    }
    let context = match known_item_context(ctx, space, item).await? {
        Some(context) => Some(context),
        None if matches!(item, Some(ItemInput::Url(_))) => None,
        None => Some(picked_space(ctx).await?),
    };
    Ok(RepoTarget {
        context,
        repository: None,
    })
}

/// Where a command about the space's items runs without asking: in a chat
/// without a repository the registered repository of the issue or PR URL
/// `item`, else the space `space` names or the chat this runs in, else the
/// current workspace or checkout, else the registered repository of `item`.
async fn known_item_context(
    ctx: &Context,
    space: Option<SpaceSelector>,
    item: Option<&ItemInput>,
) -> Result<Option<ItemContext>> {
    if space.is_none()
        && in_chat_outside_repositories(ctx).await?
        && let Some(checkout) = item_checkout(ctx, item).await?
    {
        return Ok(Some(checkout));
    }
    if space.is_some() || ui::current_chat(&space).is_some() {
        let space = ui::select_space(ctx, space, ui::Fallback::CurrentDirectory).await?;
        return Ok(Some(ItemContext::space(space)));
    }
    if let Some(context) = current_context(ctx).await? {
        return Ok(Some(context));
    }
    item_checkout(ctx, item).await
}

/// A space the user picks.
async fn picked_space(ctx: &Context) -> Result<ItemContext> {
    ensure!(ctx.interactive(), "{NO_CONTEXT}");
    let space = ui::select_space(ctx, None, ui::Fallback::CurrentDirectory).await?;
    Ok(ItemContext::space(space))
}

/// The repository a command whose `--repo` names any repository acts in.
/// The user's command runs in a registered repository's checkout, without a
/// workspace; any other repository is reached from the current context,
/// else by its URL alone, else from a repository the user picks.
pub(in crate::cli::commands) async fn select_repo_context(
    ctx: &Context,
    repo: Option<String>,
) -> Result<RepoTarget> {
    if let Some(target) = registered_target(ctx, repo.as_deref()).await? {
        return Ok(target);
    }
    // Refuse a bad repository before the picker asks for anything.
    repo.as_deref().map(ForgeRepo::check_named).transpose()?;
    let context = match current_context(ctx).await? {
        Some(context) => Some(context),
        None if named_by_url(repo.as_deref()) => None,
        None => Some(picked_repository(ctx).await?),
    };
    Ok(RepoTarget {
        context,
        repository: repo,
    })
}

/// Whether `repo` names a repository by its URL, which reaches its forge
/// from anywhere.
fn named_by_url(repo: Option<&str>) -> bool {
    repo.is_some_and(ForgeRepo::named_by_url)
}

/// The checkout of a registered repository the user picks.
async fn picked_repository(ctx: &Context) -> Result<ItemContext> {
    ensure!(ctx.interactive(), "{NO_REPOSITORY}");
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let picked = ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await)?;
    Ok(ItemContext::Checkout(picked.into()))
}

/// [`select_repo_context`] for `issue open`, which also takes the space to
/// open the issue from, a workspace or chat, and whose picker offers spaces,
/// since `--link` assigns the issue to one.
pub(in crate::cli::commands) async fn select_open_context(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
) -> Result<RepoTarget> {
    if space.is_none()
        && let Some(target) = registered_target(ctx, repo.as_deref()).await?
    {
        return Ok(target);
    }
    // Refuse a bad repository before the picker asks for anything.
    repo.as_deref().map(ForgeRepo::check_named).transpose()?;
    if space.is_none() && named_by_url(repo.as_deref()) && current_context(ctx).await?.is_none() {
        return Ok(RepoTarget {
            context: None,
            repository: repo,
        });
    }
    let context = select_space_item_context(ctx, space, None).await?;
    Ok(RepoTarget {
        context: Some(context),
        repository: repo,
    })
}

/// The checkout of the registered repository of the issue, PR or CI run URL
/// `item`, if it names one.
async fn item_checkout(ctx: &Context, item: Option<&ItemInput>) -> Result<Option<ItemContext>> {
    let Some(ItemInput::Url(item)) = item else {
        return Ok(None);
    };
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let registered = crate::repository::registered_for_item(&repos, item).await?;
    Ok(registered.map(|repo| ItemContext::Checkout(repo.id.clone())))
}

/// Whether this runs in a chat without a repository, which acts in the
/// checkout of every registered repository as its own.
async fn in_chat_outside_repositories(ctx: &Context) -> Result<bool> {
    let Some(chat) = ui::current_chat(&None) else {
        return Ok(false);
    };
    let space =
        crate::client::workspaces::present_space(&ctx.paths, &SpaceSelector::id(&chat)).await?;
    Ok(space.is_some_and(|space| space.worktree().is_none() && space.repository_id.is_none()))
}

/// The checkout of the registered repository `repo` names, where the user's
/// command, or a chat's without a repository, acts without a workspace; a
/// workspace process acts only through its own. It also names the
/// repository by its `origin`, so an item's, run's or workflow's URL of
/// another one is refused rather than followed; without an `origin` it has
/// no forge to act on.
async fn registered_target(ctx: &Context, repo: Option<&str>) -> Result<Option<RepoTarget>> {
    let Some(named) = repo else {
        return Ok(None);
    };
    if env::inherits_scope(&ctx.paths.state) && !in_chat_outside_repositories(ctx).await? {
        return Ok(None);
    }
    let Some(checkout) = registered(ctx, named).await? else {
        return Ok(None);
    };
    let origin = repository::remote_url_from_path(&checkout.path).await?;
    let origin = origin
        .with_context(|| format!("{named} has no origin remote, so it has no forge to act on"))?;
    Ok(Some(RepoTarget {
        repository: Some(origin),
        context: Some(ItemContext::Checkout(checkout.id)),
    }))
}

/// The chat this runs in, which a chat in a workspace's worktree is, else
/// the workspace the current directory is in, else the registered checkout
/// the current directory is in.
pub(in crate::cli::commands) async fn current_context(
    ctx: &Context,
) -> Result<Option<ItemContext>> {
    if let Some(chat) = ui::current_chat(&None) {
        return Ok(Some(ItemContext::space(SpaceSelector::id(&chat))));
    }
    match ui::select_workspace(ctx, None, ui::Fallback::CurrentDirectoryOnly).await {
        Ok(workspace) => {
            return Ok(Some(ItemContext::space(workspace)));
        }
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {}
        Err(error) => return Err(error),
    }
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let checkout = super::super::issues::current_checkout(&repos).await?;
    Ok(checkout.map(|repo| ItemContext::Checkout(repo.id.clone())))
}

/// The registered repository `repo` names, a path read from the current
/// directory.
pub(in crate::cli::commands) async fn named_repository(
    ctx: &Context,
    repo: &str,
) -> Result<Repository> {
    let selector = ui::repository_selector(repo.to_owned())?;
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    Ok(repository::select(&repos, selector).await?.clone())
}

/// The registered repository `repo` names, if any.
async fn registered(ctx: &Context, repo: &str) -> Result<Option<Repository>> {
    match named_repository(ctx, repo).await {
        Ok(repo) => Ok(Some(repo)),
        Err(error) if error.is::<NotRegistered>() => Ok(None),
        Err(error) => Err(error),
    }
}
