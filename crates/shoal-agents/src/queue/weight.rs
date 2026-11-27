//! Which agents are light, by their mode's `weight`, in a workspace or a chat
//! alike.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;

use crate::Agents;
use crate::config::modes::ModeName;
use crate::config::modes::Weight;
use crate::daemon::Daemon;
use crate::execution::recovery;
use crate::execution::recovery::RecordExt;
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::executions::Record;

/// The light modes of the repositories asked about, under `None` those of
/// the global config, where a chat without a repository resolves its mode,
/// and the tracked agents that run in one.
#[derive(Debug, Default)]
pub struct Light {
    modes: HashMap<Option<RepositoryId>, HashSet<String>>,
    executions: HashSet<ExecutionId>,
}

impl Light {
    /// Whether an agent of `mode`, the default mode without one, is light
    /// in `repository`, or in a chat without one.
    pub fn mode(&self, repository: Option<&RepositoryId>, mode: Option<&ModeName>) -> bool {
        let default = ModeName::DEFAULT;
        let mode = mode.unwrap_or(&default);
        (self.modes.get(&repository.cloned())).is_some_and(|light| light.contains(mode.as_str()))
    }

    /// Whether the tracked agent of `execution` is light.
    pub fn execution(&self, execution: &ExecutionId) -> bool {
        self.executions.contains(execution)
    }

    /// Whether a queue entry starts a light agent: a restore one of the
    /// light agent of its `execution`, a launch one of a light `mode` in
    /// `repository`.
    pub fn entry(
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

impl Agents {
    /// Which agents are light now: the modes of every repository with an
    /// agent or a queue entry, and of the global config when a chat without
    /// a repository has one, and the agents of every space in a light one,
    /// whether they run or, stopped, wait for their restore.
    pub async fn light(&self, daemon: &dyn Daemon) -> Result<Light> {
        let (mut executions, spaces, mut repositories) = (self.store)
            .read(move |tx| {
                let spaces: Vec<_> = (shoal_store::spaces::all_facts(tx)?.into_iter())
                    .map(|space| (space.id, space.repository))
                    .collect();
                Ok((
                    execution_modes(tx, &spaces)?,
                    spaces,
                    queue_repositories(tx)?,
                ))
            })
            .await?;
        executions.extend(self.stopped_modes(&spaces, &executions)?);
        repositories.extend(
            executions
                .iter()
                .map(|(_, repository, _)| repository.clone()),
        );
        let mut light = Light {
            modes: self.light_modes(daemon, repositories).await,
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
                // An unreadable record counts as heavy rather than failing
                // every admission pass.
                if let Some(record) = Record::load(&path).ok().flatten() {
                    stopped.push((execution, repository.clone(), Some(record.mode)));
                }
            }
        }
        Ok(stopped)
    }

    /// The light modes of each of `repositories`, `None` standing for the
    /// global config. A repository whose settings do not resolve has none, so
    /// its agents wait for slots; its config fails its commands too.
    async fn light_modes(
        &self,
        daemon: &dyn Daemon,
        repositories: HashSet<Option<RepositoryId>>,
    ) -> HashMap<Option<RepositoryId>, HashSet<String>> {
        let mut light = HashMap::new();
        for repository in repositories {
            let settings = match &repository {
                Some(id) => daemon.repository_settings(id).await,
                None => self.config.current().resolve(&Default::default()),
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

/// The agent's execution of each of `spaces`, given with their
/// repositories, with its space's repository and mode.
fn execution_modes(
    db: &Connection,
    spaces: &[(SpaceId, Option<RepositoryId>)],
) -> Result<Vec<ExecutionMode>> {
    let repositories: HashMap<_, _> = spaces.iter().cloned().collect();
    let executions = db
        .prepare("SELECT id,space_id,mode FROM executions WHERE agent IS NOT NULL")?
        .query_map([], |row| {
            Ok((
                row.get::<_, ExecutionId>(0)?,
                row.get::<_, SpaceId>(1)?,
                row.get(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((executions.into_iter())
        .filter_map(|(execution, space, mode)| {
            let repository = repositories.get(&space)?.clone();
            Some((execution, repository, mode))
        })
        .collect())
}

/// The repositories with a queue entry, `None` for an entry of a chat
/// without one.
fn queue_repositories(db: &Connection) -> Result<HashSet<Option<RepositoryId>>> {
    Ok(db
        .prepare("SELECT DISTINCT repository_id FROM agent_queue")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
