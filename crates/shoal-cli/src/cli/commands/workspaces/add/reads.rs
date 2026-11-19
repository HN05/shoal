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

    /// None for another joined machine's space, whose repository that
    /// machine reads.
    async fn space_repository(&self, selector: &SpaceSelector) -> Result<Option<RepositoryId>> {
        let spaces = crate::client::spaces::list(&self.0.paths).await?;
        if let Some(space) = spaces.find(selector) {
            return Ok(space.repository_id.clone());
        }
        match crate::cli::commands::machines::space_elsewhere(self.0, selector).await? {
            Some(_) => Ok(None),
            None => {
                Err(Refusal::new(Reason::NotFound, format!("unknown space: {selector}")).into())
            }
        }
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
