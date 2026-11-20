//! The repository a start goes in: the one it names, its link's, its
//! issue URL's, its parent's or its directory's, a terminal offering to
//! register an unregistered one, and only once its mode says it starts in
//! one, a picked one.
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
    launches::SpaceRepository,
    model::{Repository, SpaceKind},
    protocol::ConfigTarget,
};

/// Have this machine register the repository of the space `launch` goes
/// below, where another machine runs that space in a repository this one
/// has not registered and the start runs here, which the daemon refuses
/// first what the launch would refuse there for; the start then settles in
/// it as in any registered repository.
pub(super) async fn take_parents(ctx: &Context, launch: &Launch) -> Result<()> {
    match launch.parent {
        Some(_) => {
            crate::client::launches::take_parent_repository(&ctx.paths, launch.clone()).await
        }
        None => Ok(()),
    }
}

/// The registered repository `launch` starts in, found without a picker:
/// that of the workspace it names with `--in`, else the one it names,
/// its link's or its issue URL's, a terminal offering to register an
/// unregistered one, which `launch` then names by ID, else its parent's or
/// its directory's.
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

/// The repository a start in `mode`, one of the `modes` it may take, goes
/// in and the settings its agent starts under, as the daemon reads them:
/// none for a mode without a repository, under the global config's; for a
/// chat in the worktree of the workspace `launch` names, the `found`
/// repository under its own; else the `found` one with its `settings`,
/// else one a terminal picks, which `launch` then names, under its own,
/// unless only the machine that starts it has the repository of the space
/// it goes below.
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

/// The repository `launch` goes in and its settings once its arguments
/// alone call for one, whatever its mode: the `found` one with its
/// `settings`, else one a terminal picks, which `launch` then names, with
/// its own.
pub(super) async fn required(
    ctx: &Context,
    launch: &mut Launch,
    (found, settings): (Option<Repository>, Effective),
) -> Result<(Option<Repository>, Effective)> {
    if found.is_some() || !mode::requires_repository(launch)? || !picks(ctx, launch).await? {
        return Ok((found, settings));
    }
    let repo = pick(ctx, launch).await?;
    let target = ConfigTarget::Repository(repo.id.clone().into());
    let settings = target_settings(ctx, target).await?;
    Ok((Some(repo), settings))
}

/// The repository a workspace's creation `launch` goes in: the one
/// [`found`] finds, else the current or a picked one, which `launch` then
/// names.
pub(super) async fn chosen(ctx: &Context, launch: &mut Launch) -> Result<Repository> {
    match found(ctx, launch).await? {
        Some(repo) => Ok(repo),
        None => pick(ctx, launch).await,
    }
}

/// Whether a terminal picks the repository `launch` goes in where nothing
/// names one: not below another machine's space in a repository this
/// machine has not registered, whose machine the start goes to takes that
/// space's, which no repository here stands in for.
async fn picks(ctx: &Context, launch: &Launch) -> Result<bool> {
    if !ui::can_pick(ctx) {
        return Ok(false);
    }
    let Some(parent) = &launch.parent else {
        return Ok(true);
    };
    Ok(space_repository(ctx, parent).await? != SpaceRepository::Unregistered)
}

/// The current registered repository, else one a terminal picks, which
/// `launch` then names.
async fn pick(ctx: &Context, launch: &mut Launch) -> Result<Repository> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let id = issues::current_repository(ctx, repos.clone(), repositories::REPOSITORY).await?;
    launch.repository = Some(id.to_string());
    let repo = repos.into_iter().find(|repo| repo.id == id);
    anyhow::Context::with_context(repo, || format!("no repository {id}"))
}

/// The repository of the existing workspace `workspace` names.
async fn workspace_repository(
    ctx: &Context,
    workspace: &shoal_protocol::space_names::SpaceSelector,
) -> Result<shoal_ids::repository_ids::RepositoryId> {
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace.clone()).await?;
    Ok(inspection.workspace.repository().clone())
}

/// The repository `launch` names, with `--repo` or positionally.
fn named(launch: &Launch) -> Option<String> {
    let positional = match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    };
    launch.repository.clone().or(positional)
}

/// The registered repository `named`, read in the directory `launch` is
/// made in, a terminal offering to register it where this process may,
/// which then joins `repos`.
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

/// The registered repository of the issue, PR or branch `launch` links,
/// offering to register an unregistered one; none for a start on no link.
async fn linked_repository(
    ctx: &Context,
    launch: &Launch,
    repos: &mut Vec<Repository>,
) -> Result<Option<shoal_ids::repository_ids::RepositoryId>> {
    if let Some(linked) = (launch.target.as_ref()).and_then(StartTarget::linked_repository) {
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

/// Whether this process offers to register a repository: a chat starts
/// spaces only in its own repository, which is registered, and a scoped
/// process leaves registering to the user.
fn offers() -> bool {
    !crate::env::is_scoped() && crate::env::own_chat().is_none()
}
