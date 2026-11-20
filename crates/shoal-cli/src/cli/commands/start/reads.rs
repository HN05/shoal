//! What settling a start's workspace reads, asked of the daemon.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    cli::context::Context,
    config::{Effective, modes::ModeShapes},
    launches::{LaunchReads, SpaceRepository},
    model::{Repository, Workspace},
    protocol::ConfigTarget,
    refusal::{Reason, Refusal},
};

/// The daemon's state as this process reads it.
pub(in crate::cli::commands) struct DaemonReads<'a>(pub &'a Context);

impl LaunchReads for DaemonReads<'_> {
    async fn repositories(&self) -> Result<Vec<Repository>> {
        crate::client::repositories::list(&self.0.paths).await
    }

    async fn workspaces(&self) -> Result<Vec<Workspace>> {
        crate::client::spaces::list(&self.0.paths)
            .await
            .map(shoal_protocol::model::Spaces::into_workspaces)
    }

    /// For another joined machine's space, this machine's registration of
    /// the repository the hub lists it in, by its origin.
    async fn space_repository(&self, selector: &SpaceSelector) -> Result<SpaceRepository> {
        let spaces = crate::client::spaces::list(&self.0.paths).await?;
        if let Some(space) = spaces.find(selector) {
            return Ok(match &space.repository_id {
                Some(id) => SpaceRepository::Registered(id.clone()),
                None => SpaceRepository::Without,
            });
        }
        let elsewhere = crate::cli::commands::machines::space_elsewhere(self.0, selector);
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
        let repos = crate::client::repositories::list(&self.0.paths).await?;
        Ok(
            match crate::repository::with_origin(&repos, &origin).await {
                Some(repository) => SpaceRepository::Registered(repository.id.clone()),
                None => SpaceRepository::Unregistered,
            },
        )
    }

    /// The global config's settings read here, at launch, else the
    /// repository layer the daemon reads over them.
    async fn settings(&self, target: ConfigTarget) -> Result<Effective> {
        match target {
            ConfigTarget::Global => crate::config::Config::load_with_templates(&self.0.paths)?
                .resolve(&Default::default()),
            target => shoal_workers::settings::of(&self.0.paths, target).await,
        }
    }

    /// For a launch that names a machine, as that machine defines them,
    /// which the daemon asks through the hub; else as this machine's
    /// settings define them.
    async fn launch_modes(&self, launch: &Launch) -> Result<ModeShapes> {
        match launch.machine {
            Some(_) => crate::client::launches::modes(&self.0.paths, launch.clone()).await,
            None => crate::launches::shape::local_modes(self, launch).await,
        }
    }
}

/// The modes `launch` may take, each with the space it starts: as the
/// machine it names defines them, else as this machine's `settings` do.
pub(in crate::cli::commands) async fn start_modes(
    ctx: &Context,
    launch: &Launch,
    settings: &Effective,
) -> Result<ModeShapes> {
    match launch.machine {
        Some(_) => DaemonReads(ctx).launch_modes(launch).await,
        None => Ok(crate::config::modes::shapes(&settings.modes)),
    }
}
