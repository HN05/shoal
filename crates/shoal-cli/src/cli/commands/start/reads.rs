//! What settling a start reads of Shoal's state, for [`crate::launches`] to
//! decide over.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::model::Located;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    cli::context::Context,
    config::{Effective, modes::ModeShapes},
    launches::{LaunchState, SpaceRepository, shape},
    model::Repository,
    protocol::ConfigTarget,
};

pub(in crate::cli::commands) async fn launch_repository(
    ctx: &Context,
    launch: &Launch,
) -> Result<Option<Repository>> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    if let Some(repository) = shape::named_repository(&repositories, launch).await {
        return Ok(Some(repository));
    }
    let state = inherited_state(ctx, launch, repositories).await?;
    shape::inherited_repository(&state, launch).await
}

async fn inherited_state(
    ctx: &Context,
    launch: &Launch,
    repositories: Vec<Repository>,
) -> Result<LaunchState> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let parent = match &launch.parent {
        Some(parent) => space_repository(ctx, parent).await?,
        None => SpaceRepository::Without,
    };
    Ok(LaunchState {
        repositories,
        workspaces: spaces.into_workspaces(),
        parent,
    })
}

pub(in crate::cli::commands) async fn launch_settings(
    ctx: &Context,
    launch: &Launch,
) -> Result<Effective> {
    let target = match shape::workspace_target(launch) {
        Some(target) => target,
        None => repository_settings_target(ctx, launch).await?,
    };
    target_settings(ctx, target).await
}

async fn repository_settings_target(ctx: &Context, launch: &Launch) -> Result<ConfigTarget> {
    let repository = launch_repository(ctx, launch).await?;
    let workspaces = match launch.branch {
        Some(_) => crate::client::spaces::list(&ctx.paths)
            .await?
            .into_workspaces(),
        None => Vec::new(),
    };
    shape::repository_target(launch, repository, &workspaces).await
}

/// Another joined machine's space takes this machine's registration of the
/// repository the hub lists it in, by its origin.
pub(in crate::cli::commands) async fn space_repository(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<SpaceRepository> {
    let space = match crate::client::spaces::locate(&ctx.paths, selector.clone()).await? {
        Located::Here(space) => {
            return Ok(match &space.repository_id {
                Some(id) => SpaceRepository::Registered(id.clone()),
                None => SpaceRepository::Without,
            });
        }
        Located::Elsewhere { space, .. } => space,
    };
    let origin = (space.space.repository).and_then(|repository| repository.origin);
    let Some(origin) = origin else {
        return Ok(SpaceRepository::Without);
    };
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let registered = crate::repository::with_origin(&repos, &origin).await;
    Ok(match registered {
        Some(repository) => SpaceRepository::Registered(repository.id.clone()),
        None => SpaceRepository::Unregistered,
    })
}

/// A launch's mode may come from the mode and rule files beside the global
/// config.
pub(in crate::cli::commands) async fn target_settings(
    ctx: &Context,
    target: ConfigTarget,
) -> Result<Effective> {
    match target {
        ConfigTarget::Global => {
            crate::config::Config::load_with_templates(&ctx.paths)?.resolve(&Default::default())
        }
        target => shoal_workers::settings::of(&ctx.paths, target).await,
    }
}

/// The daemon asks the hub for the modes of the machine the launch names.
pub(in crate::cli::commands) async fn start_modes(
    ctx: &Context,
    launch: &Launch,
    settings: &Effective,
) -> Result<ModeShapes> {
    match launch.machine {
        Some(_) => crate::client::launches::modes(&ctx.paths, launch.clone()).await,
        None => Ok(crate::config::modes::shapes(&settings.modes)),
    }
}
