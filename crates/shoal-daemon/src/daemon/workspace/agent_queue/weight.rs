//! Which agents are light: those of a mode whose `weight` is `light` start
//! at once, hold no agent slot and are never stopped for load.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::client::{
    execution_ids::ExecutionId, executions::Record, repository_ids::RepositoryId,
    space_ids::SpaceId,
};
use crate::{
    config::modes::{ModeName, Weight},
    daemon::workspace::Manager,
    execution::recovery::{self, RecordExt},
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

    /// Whether a queue entry starts a light agent: a restore one of the
    /// light agent of its `execution`, a launch one of a light `mode` in
    /// `repository`.
    pub(crate) fn entry(
        &self,
        execution: Option<&ExecutionId>,
        repository: &RepositoryId,
        mode: Option<&ModeName>,
    ) -> bool {
        match execution {
            Some(execution) => self.execution(execution),
            None => self.mode(repository, mode),
        }
    }
}

/// A workspace agent's execution with its repository and the mode it
/// started in.
type ExecutionMode = (ExecutionId, RepositoryId, Option<ModeName>);

impl Manager {
    /// Which agents are light now: the modes of every repository with a
    /// workspace agent or a queue entry, and the workspace agents in a light
    /// one, whether they run or, stopped, wait for their restore.
    pub(crate) async fn light(&self) -> Result<Light> {
        let (mut executions, workspaces, mut repositories) = (self.store)
            .read(|tx| {
                Ok((
                    execution_modes(tx)?,
                    workspaces(tx)?,
                    queue_repositories(tx)?,
                ))
            })
            .await?;
        executions.extend(self.stopped_modes(&workspaces, &executions)?);
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

    /// The stopped agents of `workspaces` that `executions` leaves out, each
    /// with its repository and the mode its saved record keeps.
    fn stopped_modes(
        &self,
        workspaces: &[(SpaceId, RepositoryId)],
        executions: &[ExecutionMode],
    ) -> Result<Vec<ExecutionMode>> {
        let known: HashSet<&ExecutionId> = executions.iter().map(|(id, ..)| id).collect();
        let mut stopped = Vec::new();
        for (workspace, repository) in workspaces {
            for execution in recovery::agent_records(&self.paths, workspace)? {
                if known.contains(&execution) {
                    continue;
                }
                let path = recovery::record_path(&self.paths, workspace, &execution);
                // An unreadable record counts as heavy, as before weights,
                // rather than failing every admission pass.
                if let Some(record) = Record::load(&path).ok().flatten() {
                    stopped.push((execution, repository.clone(), Some(record.mode)));
                }
            }
        }
        Ok(stopped)
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

/// Each workspace with its repository.
fn workspaces(db: &Connection) -> Result<Vec<(SpaceId, RepositoryId)>> {
    Ok(db
        .prepare("SELECT id,repository_id FROM workspaces")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The repositories with a queue entry.
fn queue_repositories(db: &Connection) -> Result<HashSet<RepositoryId>> {
    Ok(db
        .prepare("SELECT DISTINCT repository_id FROM agent_queue WHERE repository_id IS NOT NULL")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
