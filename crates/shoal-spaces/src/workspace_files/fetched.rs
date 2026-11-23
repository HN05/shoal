//! Where this machine keeps the copies a caller here fetches of the files
//! of a space, which may run on another machine: beside the repository's
//! workspaces when this machine has the repository, else in `root_dir`.
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
    /// The directory, created marked as Shoal's, where this machine keeps
    /// the copies `caller` fetches of the files of the space `selector`
    /// names, among those it sees.
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

    /// The directory of fetched copies of the space `located`: its
    /// repository's when this machine registered it, else `root_dir`'s.
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

    /// The directory of fetched copies of the spaces of no repository this
    /// machine registered, in `root_dir`.
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

/// The repository of the space `located`: by its ID here, or by its
/// origin elsewhere; none for a chat without one, or a checkout without an
/// origin on another machine.
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
