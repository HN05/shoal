//! The agents a machine can launch in each of its repositories and outside
//! one, which its capacity reports carry.
use std::{collections::BTreeMap, ffi::OsString};

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

/// The settings a launch starts under outside a repository, and in each
/// repository by its origin; none for a repository whose settings do not
/// resolve.
struct Settings {
    global: Effective,
    repositories: Vec<(String, Option<Effective>)>,
}

impl Hub {
    /// The agents this machine can launch now, under the settings each
    /// launch starts under, finding their programs on the daemon's `PATH`.
    pub(super) async fn launchable(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Launchable> {
        let settings = self.launch_settings(daemon, origins).await?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        Ok(tokio::task::spawn_blocking(move || launchable_by(&settings, &path)).await?)
    }

    /// The settings a launch starts under outside a repository and in each
    /// of this machine's repositories that has an origin. A repository whose
    /// settings do not resolve launches nothing, as a launch there fails too.
    async fn launch_settings(
        &self,
        daemon: &dyn Daemon,
        origins: &mut Origins,
    ) -> Result<Settings> {
        let global = self.config.current().resolve(&Default::default())?;
        let mut repositories = Vec::new();
        for repository in daemon.repositories().await? {
            let Some(origin) = origins.of(&repository).await.origin else {
                continue;
            };
            let settings = daemon.repository_settings(&repository.id).await.ok();
            repositories.push((origin, settings));
        }
        Ok(Settings {
            global,
            repositories,
        })
    }
}

/// The agents `settings` name whose programs `path` finds, each list
/// sorted. Two registrations of one origin launch what either does.
fn launchable_by(settings: &Settings, path: &OsString) -> Launchable {
    let installed = |settings: &Effective| {
        (crate::agents::installed_agents(settings, path).into_iter())
            .map(|agent| agent.name().to_owned())
    };
    let mut by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (origin, settings) in &settings.repositories {
        let agents = by_origin.entry(origin.clone()).or_default();
        agents.extend(settings.iter().flat_map(installed));
    }
    let repositories = (by_origin.into_iter())
        .map(|(origin, mut agents)| {
            agents.sort();
            agents.dedup();
            Registered {
                origin,
                agents,
                ..Registered::default()
            }
        })
        .collect();
    let mut agents: Vec<String> = installed(&settings.global).collect();
    agents.sort();
    Launchable {
        repositories,
        agents,
    }
}

#[cfg(test)]
mod tests;
