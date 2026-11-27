//! The agents a machine can launch in each of its repositories and outside
//! one, which its capacity reports carry.
use std::collections::BTreeMap;

use anyhow::Result;
use shoal_schedule::placement::snapshot::Registered;

use super::summaries::Origins;
use crate::{Hub, config::Effective, daemon::Daemon};

/// What a machine can launch: in each of its repositories, by origin, and
/// outside one.
#[derive(Debug, Default)]
pub(super) struct Launchable {
    pub repositories: Vec<Registered>,
    pub agents: Vec<String>,
}

impl Hub {
    /// The agents this machine can launch now, resolving each repository's
    /// settings as a launch there does and finding their programs on the
    /// daemon's `PATH`. A repository whose settings do not resolve launches
    /// none, as a launch there fails too.
    pub(super) async fn launchable(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Launchable> {
        let global = self.config.current().resolve(&Default::default())?;
        let mut repositories = Vec::new();
        for repository in daemon.repositories().await? {
            let Some(origin) = origins.of(&repository).await.origin else {
                continue;
            };
            let settings = daemon.repository_settings(&repository.id).await.ok();
            repositories.push((origin, settings));
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = move || {
            let installed = |settings: &Effective| {
                (crate::agents::installed_agents(settings, &path).into_iter())
                    .map(|agent| agent.name().to_owned())
            };
            // Two registrations of one origin launch what either does.
            let mut by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (origin, settings) in &repositories {
                let agents = by_origin.entry(origin.clone()).or_default();
                agents.extend(settings.iter().flat_map(installed));
            }
            let repositories = (by_origin.into_iter())
                .map(|(origin, mut agents)| {
                    agents.sort();
                    agents.dedup();
                    Registered { origin, agents }
                })
                .collect();
            let mut agents: Vec<String> = installed(&global).collect();
            agents.sort();
            Launchable {
                repositories,
                agents,
            }
        };
        Ok(tokio::task::spawn_blocking(found).await?)
    }
}

#[cfg(test)]
mod tests;
