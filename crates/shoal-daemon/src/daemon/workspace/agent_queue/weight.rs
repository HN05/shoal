//! Which agents are light: those of a mode whose `weight` is `light` start
//! at once, hold no agent slot and are never stopped for load, in a
//! workspace or a chat alike.
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

/// The light modes of the repositories asked about, under `None` those of
/// the global config, where a chat without a repository resolves its mode,
/// and the tracked agents that run in one.
#[derive(Debug, Default)]
pub(crate) struct Light {
    modes: HashMap<Option<RepositoryId>, HashSet<String>>,
    executions: HashSet<ExecutionId>,
}

impl Light {
    /// Whether an agent of `mode`, the default mode without one, is light
    /// in `repository`, or in a chat without one.
    pub(crate) fn mode(&self, repository: Option<&RepositoryId>, mode: Option<&ModeName>) -> bool {
        let default = ModeName::DEFAULT;
        let mode = mode.unwrap_or(&default);
        (self.modes.get(&repository.cloned())).is_some_and(|light| light.contains(mode.as_str()))
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
        repository: Option<&RepositoryId>,
        mode: Option<&ModeName>,
    ) -> bool {
        match execution {
            Some(execution) => self.execution(execution),
            None => self.mode(repository, mode),
        }
    }
}

/// A space's agent's execution with its space's repository, none for a
/// chat without one, and the mode it started in.
type ExecutionMode = (ExecutionId, Option<RepositoryId>, Option<ModeName>);

impl Manager {
    /// Which agents are light now: the modes of every repository with an
    /// agent or a queue entry, and of the global config when a chat without
    /// a repository has one, and the agents of every space in a light one,
    /// whether they run or, stopped, wait for their restore.
    pub(crate) async fn light(&self) -> Result<Light> {
        let (mut executions, spaces, mut repositories) = (self.store)
            .read(|tx| Ok((execution_modes(tx)?, spaces(tx)?, queue_repositories(tx)?)))
            .await?;
        executions.extend(self.stopped_modes(&spaces, &executions)?);
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
            if light.mode(repository.as_ref(), mode.as_ref()) {
                light.executions.insert(execution);
            }
        }
        Ok(light)
    }

    /// The stopped agents of `spaces` that `executions` leaves out, each
    /// with its repository and the mode its saved record keeps.
    fn stopped_modes(
        &self,
        spaces: &[(SpaceId, Option<RepositoryId>)],
        executions: &[ExecutionMode],
    ) -> Result<Vec<ExecutionMode>> {
        let known: HashSet<&ExecutionId> = executions.iter().map(|(id, ..)| id).collect();
        let mut stopped = Vec::new();
        for (space, repository) in spaces {
            for execution in recovery::agent_records(&self.paths, space)? {
                if known.contains(&execution) {
                    continue;
                }
                let path = recovery::record_path(&self.paths, space, &execution);
                // An unreadable record counts as heavy, as before weights,
                // rather than failing every admission pass.
                if let Some(record) = Record::load(&path).ok().flatten() {
                    stopped.push((execution, repository.clone(), Some(record.mode)));
                }
            }
        }
        Ok(stopped)
    }

    /// The light modes of each of `repositories`, `None` standing for the
    /// global config. A repository whose settings do not resolve has none,
    /// so its agents wait for slots as before; its config fails its
    /// commands too.
    async fn light_modes(
        &self,
        repositories: HashSet<Option<RepositoryId>>,
    ) -> HashMap<Option<RepositoryId>, HashSet<String>> {
        let mut light = HashMap::new();
        for repository in repositories {
            let settings = match &repository {
                Some(id) => self.repository_settings(id).await,
                None => self.config().resolve(&Default::default()),
            };
            let Ok(settings) = settings else {
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

/// Each space's agent's execution, with its repository and mode.
fn execution_modes(db: &Connection) -> Result<Vec<ExecutionMode>> {
    Ok(db
        .prepare(
            "SELECT e.id,s.repository_id,e.mode FROM executions e
             JOIN spaces s ON s.id=e.space_id WHERE e.agent IS NOT NULL",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Each space with its repository.
fn spaces(db: &Connection) -> Result<Vec<(SpaceId, Option<RepositoryId>)>> {
    Ok(db
        .prepare("SELECT id,repository_id FROM spaces")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The repositories with a queue entry, `None` for an entry of a chat
/// without one.
fn queue_repositories(db: &Connection) -> Result<HashSet<Option<RepositoryId>>> {
    Ok(db
        .prepare("SELECT DISTINCT repository_id FROM agent_queue")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
