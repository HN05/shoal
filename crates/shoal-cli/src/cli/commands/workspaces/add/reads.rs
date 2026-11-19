//! What settling a start's workspace reads, asked of the daemon.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    cli::context::Context,
    config::Effective,
    launches::LaunchReads,
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

    async fn space_repository(&self, selector: &SpaceSelector) -> Result<Option<RepositoryId>> {
        let spaces = crate::client::spaces::list(&self.0.paths).await?;
        let space = spaces
            .find(selector)
            .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown space: {selector}")))?;
        Ok(space.repository_id)
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
}
