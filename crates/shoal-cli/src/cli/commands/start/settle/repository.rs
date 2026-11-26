//! The repository a start goes in.
use anyhow::Result;
use shoal_protocol::launches::{ItemChoice, Launch, SpaceChoice, StartTarget};

use super::mode;
use crate::{
    cli::{
        commands::{
            issues, repositories,
            start::{launch_repository, space_repository, target_settings},
        },
        context::Context,
        ui,
    },
    config::{
        Effective,
        modes::{ModeName, ModeShapes},
    },
    forge::items::reference::ItemInput,
    launches::{SpaceRepository, fit},
    model::{Repository, SpaceKind},
    protocol::ConfigTarget,
};

/// Another machine may run the space `launch` goes below in a repository this
/// one has not registered. The start then settles in it here as in any
/// registered repository.
pub(super) async fn take_parents(ctx: &Context, launch: &Launch) -> Result<()> {
    match launch.parent {
        Some(_) => {
            crate::client::launches::take_parent_repository(&ctx.paths, launch.clone()).await
        }
        None => Ok(()),
    }
}

/// A terminal offers to register an unregistered one, which `launch` then names
/// by ID.
pub(super) async fn found(ctx: &Context, launch: &mut Launch) -> Result<Option<Repository>> {
    let mut repos = crate::client::repositories::list(&ctx.paths).await?;
    if let Some(workspace) = (launch.in_workspace.as_ref()).and_then(SpaceChoice::named) {
        let id = workspace_repository(ctx, workspace).await?;
        return Ok(repos.into_iter().find(|repo| repo.id == id));
    }
    let named = match named(launch) {
        Some(named) => Some(named_repository(ctx, named, launch, &mut repos).await?),
        None => linked_repository(ctx, launch, &mut repos).await?,
    };
    if let Some(id) = named {
        // It keeps the place it was named in, which refusals name.
        match &mut launch.target {
            Some(StartTarget::Repository(named)) => *named = id.to_string(),
            _ => launch.repository = Some(id.to_string()),
        }
        return Ok(repos.into_iter().find(|repo| repo.id == id));
    }
    launch_repository(ctx, launch).await
}

/// None for a mode without a repository, under the global config's. A picked
/// repository is named in `launch`.
pub(super) async fn settled(
    ctx: &Context,
    launch: &mut Launch,
    (mode, modes): (&ModeName, &ModeShapes),
    (found, settings): (Option<Repository>, Effective),
) -> Result<(Option<Repository>, Effective)> {
    let configured = mode::configured(launch, mode, modes)?;
    if configured.is_some_and(|mode| !mode.repository) {
        let settings = target_settings(ctx, ConfigTarget::Global).await?;
        return Ok((None, settings));
    }
    let hosted = launch.in_workspace.is_some()
        && configured.is_some_and(|mode| mode.kind == SpaceKind::Chat);
    if let (true, Some(repo)) = (hosted, &found) {
        let target = ConfigTarget::Repository(repo.id.clone().into());
        let settings = target_settings(ctx, target).await?;
        return Ok((found, settings));
    }
    if found.is_some() || configured.is_none() || !picks(ctx, launch).await? {
        return Ok((found, settings));
    }
    let repo = pick(ctx, launch).await?;
    let target = ConfigTarget::Repository(repo.id.clone().into());
    let settings = target_settings(ctx, target).await?;
    Ok((Some(repo), settings))
}

pub(super) async fn required(
    ctx: &Context,
    launch: &mut Launch,
    (found, settings): (Option<Repository>, Effective),
) -> Result<(Option<Repository>, Effective)> {
    if found.is_some() || !fit::requires_repository(launch)? || !picks(ctx, launch).await? {
        return Ok((found, settings));
    }
    let repo = pick(ctx, launch).await?;
    let target = ConfigTarget::Repository(repo.id.clone().into());
    let settings = target_settings(ctx, target).await?;
    Ok((Some(repo), settings))
}

pub(super) async fn chosen(ctx: &Context, launch: &mut Launch) -> Result<Repository> {
    match found(ctx, launch).await? {
        Some(repo) => Ok(repo),
        None => pick(ctx, launch).await,
    }
}

/// Not below another machine's space in a repository this machine has not
/// registered: the machine the start goes to takes that space's, which no
/// repository here stands in for.
async fn picks(ctx: &Context, launch: &Launch) -> Result<bool> {
    if !ui::can_pick(ctx) {
        return Ok(false);
    }
    let Some(parent) = &launch.parent else {
        return Ok(true);
    };
    Ok(space_repository(ctx, parent).await? != SpaceRepository::Unregistered)
}

async fn pick(ctx: &Context, launch: &mut Launch) -> Result<Repository> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let id = issues::current_repository(ctx, repos.clone(), repositories::REPOSITORY).await?;
    launch.repository = Some(id.to_string());
    let repo = repos.into_iter().find(|repo| repo.id == id);
    anyhow::Context::with_context(repo, || format!("no repository {id}"))
}

async fn workspace_repository(
    ctx: &Context,
    workspace: &shoal_protocol::space_names::SpaceSelector,
) -> Result<shoal_ids::repository_ids::RepositoryId> {
    let inspection = crate::client::workspaces::inspect(workspace.clone())
        .send(&ctx.paths)
        .await?;
    Ok(inspection.workspace.repository().clone())
}

fn named(launch: &Launch) -> Option<String> {
    let positional = match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    };
    launch.repository.clone().or(positional)
}

async fn named_repository(
    ctx: &Context,
    named: String,
    launch: &Launch,
    repos: &mut Vec<Repository>,
) -> Result<shoal_ids::repository_ids::RepositoryId> {
    let selector = crate::repository::named_in(named, launch.directory.as_deref())?;
    if offers()
        && let Some(registered) = repositories::offer_unregistered(ctx, &selector).await?
    {
        repos.push(registered.clone());
        return Ok(registered.id);
    }
    Ok(crate::repository::select(repos, selector).await?.id.clone())
}

async fn linked_repository(
    ctx: &Context,
    launch: &Launch,
    repos: &mut Vec<Repository>,
) -> Result<Option<shoal_ids::repository_ids::RepositoryId>> {
    let flagged = [&launch.pr, &launch.ci]
        .into_iter()
        .find_map(|item| match item {
            Some(ItemChoice::Named(ItemInput::Url(item))) => Some(item.repository()),
            _ => None,
        });
    let positional = (launch.target.as_ref()).and_then(StartTarget::linked_repository);
    if let Some(linked) = positional.or(flagged) {
        let retry = "shoal start <link> --repo <repository>";
        return issues::registered_remote(ctx, repos, linked.clone(), retry)
            .await
            .map(Some);
    }
    match &launch.issue {
        Some(ItemChoice::Named(ItemInput::Url(issue))) => {
            issues::repository_for(ctx, repos, issue, offers())
                .await
                .map(Some)
        }
        _ => Ok(None),
    }
}

/// A chat starts spaces only in its own repository, which is registered, and a
/// scoped process leaves registering to the user.
fn offers() -> bool {
    !crate::env::is_scoped() && crate::env::own_chat().is_none()
}
