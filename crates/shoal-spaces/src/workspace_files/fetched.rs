//! Where this machine keeps the copies a caller fetches of a space's files:
//! beside the repository's workspaces when this machine has the repository,
//! else in `root_dir`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result};
use shoal_protocol::{files::FetchedDirectory, model::Located, repository::Selector};

use crate::{
    repository::NotRegistered,
    scope::Caller,
    shoal_dirs::{Contents, ShoalDir},
};
use shoal_protocol::space_names::SpaceSelector;

impl Spaces {
    /// Created marked as Shoal's. The space may run on another machine.
    pub async fn fetched_directory(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<FetchedDirectory> {
        let located = self.locate_space(daemon, caller, selector).await?;
        let home = self.fetched_home(daemon, &located).await?;
        let path = home.root().join(located.id().as_str());
        let created = path.clone();
        tokio::task::spawn_blocking(move || {
            home.create()?;
            std::fs::create_dir_all(&created)
                .with_context(|| format!("create {}", created.display()))
        })
        .await??;
        Ok(FetchedDirectory { path })
    }

    async fn fetched_home(&self, daemon: &dyn Daemon, located: &Located) -> Result<ShoalDir> {
        if let Some(selector) = repository_of(located)? {
            match daemon.repository(selector).await {
                Ok(repository) => return Ok(ShoalDir::of(&repository, Contents::Fetched)),
                Err(error) if NotRegistered::refused(&error) => {}
                Err(error) => return Err(error),
            }
        }
        self.fetched_outside_repositories()
    }

    fn fetched_outside_repositories(&self) -> Result<ShoalDir> {
        let root_dir = self.config.current().root_dir(&self.paths)?;
        let owner = self.root_dir_owner();
        Ok(ShoalDir::outside_repositories(
            &root_dir,
            &owner,
            Contents::Fetched,
        ))
    }
}

/// By its ID here, or by its origin elsewhere. None for a chat without a
/// repository, or a checkout without an origin on another machine.
fn repository_of(located: &Located) -> Result<Option<Selector>> {
    match located {
        Located::Here(space) => Ok(space.repository_id.clone().map(Selector::from)),
        Located::Elsewhere { space, .. } => (space.space.repository.as_ref())
            .and_then(|repository| repository.origin.clone())
            .map(crate::repository::named)
            .transpose(),
    }
}

pub mod copies;
pub mod expiry;

#[cfg(test)]
mod tests;
