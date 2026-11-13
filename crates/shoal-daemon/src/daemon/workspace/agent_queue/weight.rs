//! Which agents are light: those of a mode whose `weight` is `light` start
//! at once, hold no agent slot and are never stopped for load.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::client::{execution_ids::ExecutionId, repository_ids::RepositoryId};
use crate::{
    config::modes::{ModeName, Weight},
    daemon::workspace::Manager,
};

/// The light modes of the repositories asked about, and the tracked agents
/// that run in one.
#[derive(Debug, Default)]
pub(crate) struct Light {
    modes: HashMap<RepositoryId, HashSet<String>>,
    executions: HashSet<ExecutionId>,
}

impl Light {
    /// Whether an agent of `mode`, the default mode without one, is light
    /// in `repository`.
    pub(crate) fn mode(&self, repository: &RepositoryId, mode: Option<&ModeName>) -> bool {
        let default = ModeName::DEFAULT;
        let mode = mode.unwrap_or(&default);
        (self.modes.get(repository)).is_some_and(|light| light.contains(mode.as_str()))
    }

    /// Whether the tracked agent of `execution` is light.
    pub(crate) fn execution(&self, execution: &ExecutionId) -> bool {
        self.executions.contains(execution)
    }
}

/// A workspace agent's execution with its repository and the mode it
/// started in.
type ExecutionMode = (ExecutionId, RepositoryId, Option<ModeName>);

impl Manager {
    /// Which agents are light now: the modes of every repository with a
    /// workspace agent or a queue entry, and the workspace agents in a light
    /// one.
    pub(crate) async fn light(&self) -> Result<Light> {
        let (executions, mut repositories) = (self.store)
            .read(|tx| Ok((execution_modes(tx)?, queue_repositories(tx)?)))
            .await?;
        repositories.extend(
            executions
                .iter()
                .map(|(_, repository, _)| repository.clone()),
        );
        let mut light = Light {
            modes: self.light_modes(repositories).await,
            executions: HashSet::new(),
        };
        for (execution, repository, mode) in executions {
            if light.mode(&repository, mode.as_ref()) {
                light.executions.insert(execution);
            }
        }
        Ok(light)
    }

    /// Whether an agent of `mode` is light in `repository`.
    pub(crate) async fn light_mode(
        &self,
        repository: &RepositoryId,
        mode: Option<&ModeName>,
    ) -> bool {
        let modes = self.light_modes(HashSet::from([repository.clone()])).await;
        let light = Light {
            modes,
            ..Light::default()
        };
        light.mode(repository, mode)
    }

    /// The light modes of each of `repositories`. A repository whose
    /// settings do not resolve has none, so its agents wait for slots as
    /// before; its config fails its commands too.
    async fn light_modes(
        &self,
        repositories: HashSet<RepositoryId>,
    ) -> HashMap<RepositoryId, HashSet<String>> {
        let mut light = HashMap::new();
        for repository in repositories {
            let Ok(settings) = self.repository_settings(&repository).await else {
                continue;
            };
            let modes = (settings.modes.iter())
                .filter(|(_, mode)| mode.weight == Weight::Light)
                .map(|(name, _)| name.clone())
                .collect();
            light.insert(repository, modes);
        }
        light
    }
}

/// Each workspace agent's execution, with its repository and mode.
fn execution_modes(db: &Connection) -> Result<Vec<ExecutionMode>> {
    Ok(db
        .prepare(
            "SELECT e.id,w.repository_id,e.mode FROM executions e
             JOIN workspaces w ON w.id=e.space_id WHERE e.agent IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The repositories with a queue entry.
fn queue_repositories(db: &Connection) -> Result<HashSet<RepositoryId>> {
    Ok(db
        .prepare("SELECT DISTINCT repository_id FROM agent_queue")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
