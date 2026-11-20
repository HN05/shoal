//! The triggers the config defines: the global config's, and each
//! registered repository's, from its checkout's file and its local config.
use anyhow::{Result, bail};
use shoal_core::triggers::{Trigger, Triggers};
use shoal_ids::repository_ids::RepositoryId;

use crate::{Runner, daemon::Daemon, log};

/// The key of the global config's triggers' runtime state, which no
/// repository's ID equals.
const GLOBAL: &str = "global";

/// A trigger as the config defines it, with where.
#[derive(Debug, Clone)]
pub struct Configured {
    /// The registered repository whose config defines it; none for the
    /// global config's.
    pub repository: Option<RepositoryId>,
    pub name: String,
    pub trigger: Trigger,
}

impl Configured {
    /// The key of its runtime state: its repository's ID, else `global`.
    pub fn scope(&self) -> String {
        (self.repository.as_ref()).map_or_else(|| GLOBAL.to_owned(), ToString::to_string)
    }

    /// How a message names it.
    pub fn label(&self) -> String {
        match &self.repository {
            Some(repository) => format!("trigger {} of repository {repository}", self.name),
            None => format!("trigger {} of the global config", self.name),
        }
    }
}

impl Runner {
    /// Every configured trigger, the global config's first. A repository
    /// whose config cannot be read has none until it can.
    pub(crate) async fn configured_triggers(&self, daemon: &dyn Daemon) -> Vec<Configured> {
        let mut triggers = global(self.config().triggers());
        let repositories = match daemon.repositories().await {
            Ok(repositories) => repositories.into_iter().map(|repository| repository.id),
            Err(error) => {
                log!("cannot list the repositories whose triggers fire: {error:#}");
                return triggers;
            }
        };
        for repository in repositories {
            let found = self.repository_triggers(daemon, &repository).await;
            let error = found.as_ref().err().map(|error| format!("{error:#}"));
            if self.triggers.read(&repository, error.clone()) {
                log!(
                    "cannot read the triggers of repository {repository}: {}",
                    error.unwrap_or_default()
                );
            }
            triggers.extend(found.unwrap_or_default());
        }
        triggers
    }

    /// The triggers of `repository`, if any, after the global config's.
    pub(crate) async fn triggers_of(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<Configured>> {
        let mut triggers = global(self.config().triggers());
        if let Some(repository) = repository {
            triggers.extend(self.repository_triggers(daemon, repository).await?);
        }
        Ok(triggers)
    }

    /// The trigger `name` of `repository`, else of the global config.
    pub(crate) async fn named_trigger(
        &self,
        daemon: &dyn Daemon,
        repository: Option<&RepositoryId>,
        name: &str,
    ) -> Result<Configured> {
        let triggers = self.triggers_of(daemon, repository).await?;
        // A repository's trigger, listed last, stands before the global
        // config's.
        match (triggers.into_iter()).rfind(|trigger| trigger.name == name) {
            Some(trigger) => Ok(trigger),
            None => match repository {
                Some(repository) => bail!(
                    "no trigger {name} in repository {repository} or the global config; see shoal trigger ls"
                ),
                None => bail!(
                    "no trigger {name} in the global config; run this in a repository or pass --repo for a repository's, see shoal trigger ls"
                ),
            },
        }
    }

    /// The triggers the registered `repository`'s config defines: its
    /// tracked file's and its local config's, or only the local config's
    /// for a contribution, whose tracked file the user does not maintain.
    async fn repository_triggers(
        &self,
        daemon: &dyn Daemon,
        repository: &RepositoryId,
    ) -> Result<Vec<Configured>> {
        let mut layers = daemon.repository_layers(repository).await?;
        if daemon.repository(repository).await?.contribution {
            layers.worktree_file.triggers.clear();
        }
        let settings = self.config().resolve(&layers)?;
        Ok((settings.triggers.iter())
            .map(|(name, trigger)| Configured {
                repository: Some(repository.clone()),
                name: name.clone(),
                trigger: trigger.clone(),
            })
            .collect())
    }
}

/// The global config's `triggers`.
fn global(triggers: &Triggers) -> Vec<Configured> {
    (triggers.iter())
        .map(|(name, trigger)| Configured {
            repository: None,
            name: name.clone(),
            trigger: trigger.clone(),
        })
        .collect()
}
