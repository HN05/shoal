//! What settling a start's workspace reads, asked of the daemon.
use anyhow::Result;

use crate::{
    cli::context::Context,
    client::workspaces::PlannedWorkspace,
    config::Effective,
    launches::LaunchReads,
    model::{Repository, Workspace},
    protocol::ConfigTarget,
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

    /// The global config's settings read here, at launch, else the
    /// repository layer the daemon reads over them.
    async fn settings(&self, target: ConfigTarget) -> Result<Effective> {
        match target {
            ConfigTarget::Global => crate::config::Config::load_with_templates(&self.0.paths)?
                .resolve(&Default::default()),
            target => shoal_workers::settings::of(&self.0.paths, target).await,
        }
    }

    async fn check_opens(
        &self,
        repository: &crate::repository::Selector,
        existing: &str,
    ) -> Result<()> {
        let planned = PlannedWorkspace {
            repository: repository.clone(),
            branch: None,
            existing: Some(existing.to_owned()),
            base: None,
            git_profile: None,
            path: None,
            parent: None,
            options: Default::default(),
        };
        crate::client::workspaces::check_creation(&self.0.paths, planned).await
    }
}
