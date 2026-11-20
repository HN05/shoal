//! What settling a start reads of Shoal's state, asked of the daemon, for
//! [`crate::launches`] to decide over.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::model::Spaces;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    cli::context::Context,
    config::{Effective, modes::ModeShapes},
    launches::{LaunchState, SpaceRepository, shape},
    model::Repository,
    protocol::ConfigTarget,
    refusal::{Reason, Refusal},
};

/// The registered repository `launch` starts in, as
/// [`shape::launch_repository`] finds it, which reads the space it goes
/// below and the workspaces only where it names or links none.
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

/// What [`shape::inherited_repository`] reads of `launch` beside the
/// registered `repositories`: the workspaces this process sees and the
/// repository of the space it goes below, from one listing of the spaces.
async fn inherited_state(
    ctx: &Context,
    launch: &Launch,
    repositories: Vec<Repository>,
) -> Result<LaunchState> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let parent = match &launch.parent {
        Some(parent) => listed_space_repository(ctx, &spaces, parent).await?,
        None => SpaceRepository::Without,
    };
    Ok(LaunchState {
        repositories,
        workspaces: spaces.into_workspaces(),
        parent,
    })
}

/// The settings whose modes decide the kind of `launch`, those of the
/// target [`shape::settings_target`] finds.
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

/// The target [`shape::repository_target`] finds for `launch` in the
/// repository it starts in, which reads the workspaces only for a launch
/// that names a branch.
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

/// The repository of the space `selector` names.
pub(in crate::cli::commands) async fn space_repository(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<SpaceRepository> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    listed_space_repository(ctx, &spaces, selector).await
}

/// The repository of the space `selector` names among `spaces`, else, for
/// another joined machine's space, this machine's registration of the
/// repository the hub lists it in, by its origin.
async fn listed_space_repository(
    ctx: &Context,
    spaces: &Spaces,
    selector: &SpaceSelector,
) -> Result<SpaceRepository> {
    if let Some(space) = spaces.find(selector) {
        return Ok(match &space.repository_id {
            Some(id) => SpaceRepository::Registered(id.clone()),
            None => SpaceRepository::Without,
        });
    }
    let elsewhere = crate::cli::commands::machines::space_elsewhere(ctx, selector);
    let Some((remote, _)) = elsewhere.await? else {
        let unknown = format!("unknown space: {selector}");
        return Err(Refusal::new(Reason::NotFound, unknown).into());
    };
    let origin = remote
        .space
        .repository
        .and_then(|repository| repository.origin);
    let Some(origin) = origin else {
        return Ok(SpaceRepository::Without);
    };
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    Ok(
        match crate::repository::with_origin(&repos, &origin).await {
            Some(repository) => SpaceRepository::Registered(repository.id.clone()),
            None => SpaceRepository::Unregistered,
        },
    )
}

/// The settings of `target`, with the mode and rule files beside the
/// global config, which a launch's mode may come from: the global config's
/// read here, at launch, else the repository layer the daemon reads over
/// them.
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

/// The modes `launch` may take, each with the space it starts: as the
/// machine it names defines them, which the daemon asks through the hub,
/// else as this machine's `settings` do.
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
