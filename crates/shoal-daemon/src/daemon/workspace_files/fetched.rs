//! Where this machine keeps the copies a caller here fetches of the files
//! of a space, which may run on another machine: beside the repository's
//! workspaces when this machine has the repository, else in `root_dir`.
use anyhow::{Context as _, Result};
use shoal_protocol::{files::FetchedDirectory, model::Located, repository::Selector};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    repository::NotRegistered,
    shoal_dirs::{Contents, ShoalDir},
};
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The directory, created marked as Shoal's, where this machine keeps
    /// the copies `caller` fetches of the files of the space `selector`
    /// names, among those it sees.
    pub(crate) async fn fetched_directory(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<FetchedDirectory> {
        let located = self.locate_space(caller, selector).await?;
        let home = self.fetched_home(&located).await?;
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
    async fn fetched_home(&self, located: &Located) -> Result<ShoalDir> {
        if let Some(selector) = repository_of(located)? {
            match self.repositories.repository(selector).await {
                Ok(repository) => return Ok(ShoalDir::of(&repository, Contents::Fetched)),
                Err(error) if NotRegistered::refused(&error) => {}
                Err(error) => return Err(error),
            }
        }
        let root_dir = self.config().root_dir(&self.paths)?;
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

#[cfg(test)]
mod tests;
