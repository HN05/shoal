//! The triggers the config defines: the global config's, and each
//! registered repository's, from its checkout's file and its local config.
use anyhow::{Result, bail};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    daemon::{log, workspace::Manager},
    protocol::ConfigTarget,
    triggers::{Trigger, Triggers},
};

/// The key of the global config's triggers' runtime state, which no
/// repository's ID equals.
const GLOBAL: &str = "global";

/// A trigger as the config defines it, with where.
#[derive(Debug, Clone)]
pub(crate) struct Configured {
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

impl Manager {
    /// Every configured trigger, the global config's first. A repository
    /// whose config cannot be read has none until it can.
    pub(crate) async fn configured_triggers(&self) -> Vec<Configured> {
        let mut triggers = global(self.config().triggers());
        let repositories = match self.repositories().await {
            Ok(repositories) => repositories.into_iter().map(|repository| repository.id),
            Err(error) => {
                log!("cannot list the repositories whose triggers fire: {error:#}");
                return triggers;
            }
        };
        for repository in repositories {
            match self.repository_triggers(&repository).await {
                Ok(found) => triggers.extend(found),
                Err(error) => {
                    log!("cannot read the triggers of repository {repository}: {error:#}")
                }
            }
        }
        triggers
    }

    /// The triggers of `repository`, if any, after the global config's.
    pub(crate) async fn triggers_of(
        &self,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<Configured>> {
        let mut triggers = global(self.config().triggers());
        if let Some(repository) = repository {
            triggers.extend(self.repository_triggers(repository).await?);
        }
        Ok(triggers)
    }

    /// The trigger `name` of `repository`, else of the global config.
    pub(crate) async fn named_trigger(
        &self,
        repository: Option<&RepositoryId>,
        name: &str,
    ) -> Result<Configured> {
        let triggers = self.triggers_of(repository).await?;
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

    /// The triggers the registered `repository`'s config defines.
    async fn repository_triggers(&self, repository: &RepositoryId) -> Result<Vec<Configured>> {
        let target = ConfigTarget::Repository(repository.clone().into());
        let layers = self.config_layers_for(target).await?;
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
