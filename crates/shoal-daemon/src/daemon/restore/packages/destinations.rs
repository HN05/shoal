//! The repositories a package's spaces go to: the registered one the import
//! names for its own space, else the one with each space's origin, which
//! the import registers as a clone of that origin when none here has it.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};

use crate::daemon::{manager::Manager, scope::Caller};
use crate::{model::RemovedSpace, repository::NotRegistered, transfer::Manifest};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::repository::Selector;

/// The repository here of each repository a package's spaces were in, by
/// the ID it had there.
#[derive(Default)]
pub(super) struct Destinations {
    /// Where the package's own space goes; none for a chat without one.
    own: Option<RepositoryId>,
    here: BTreeMap<RepositoryId, RepositoryId>,
}

impl Destinations {
    /// The repository the package's own space goes to.
    pub(super) fn own(&self) -> Option<&RepositoryId> {
        self.own.as_ref()
    }

    /// The repository the packaged `space` goes to; none for a chat without
    /// one.
    pub(super) fn of(&self, space: &RemovedSpace) -> Option<Selector> {
        let there = space.repository_id.as_ref()?;
        self.here.get(there).map(|here| here.clone().into())
    }
}

impl Manager {
    /// The repositories the spaces of the package `manifest` go to: the
    /// one `repository` names for its own space and every space of the
    /// same repository, else the one with the origin each space records.
    /// `caller`, when scoped, imports its own space only into its own
    /// repository and clones none.
    pub(super) async fn destinations(
        &self,
        manifest: &Manifest,
        repository: Option<&Selector>,
        caller: Option<&Caller>,
    ) -> Result<Destinations> {
        let mut destinations = Destinations::default();
        if let Some(there) = &manifest.space.record.repository_id {
            let here = match repository {
                Some(named) => self.repository(named).await?.id,
                None => self.own_destination(manifest, caller).await?,
            };
            destinations.own = Some(here.clone());
            destinations.here.insert(there.clone(), here);
        }
        for carried in &manifest.carried {
            let Some(there) = &carried.space.record.repository_id else {
                continue;
            };
            if destinations.here.contains_key(there) {
                continue;
            }
            let name = &carried.space.record.name;
            let here = async {
                let origin =
                    (carried.remote_url.clone()).context("its repository has no origin")?;
                self.with_origin(origin, caller).await
            };
            let here = (here.await).with_context(|| format!("{name} goes with it, but"))?;
            destinations.here.insert(there.clone(), here);
        }
        Ok(destinations)
    }

    /// The repository with the origin of the package `manifest`, as
    /// [`Self::with_origin`] finds it, a scoped `caller`'s own.
    async fn own_destination(
        &self,
        manifest: &Manifest,
        caller: Option<&Caller>,
    ) -> Result<RepositoryId> {
        let origin = (manifest.remote_url.clone()).context(
            "the exported repository has no origin; select the repository to import into with --repo",
        )?;
        let Some(caller) = caller else {
            return self.with_origin(origin, None).await;
        };
        const REFUSAL: &str = "scoped processes import only into their own repository";
        let selector = crate::repository::named(origin)?;
        let own = self.own_repository(caller, &selector, REFUSAL).await?;
        Ok(self.repository(&own).await?.id)
    }

    /// The registered repository with `origin`, which the user's import
    /// registers as a clone of it when none here has it; a scoped `caller`
    /// clones none.
    async fn with_origin(&self, origin: String, caller: Option<&Caller>) -> Result<RepositoryId> {
        let selector = crate::repository::named(origin.clone())?;
        match self.repository(selector).await {
            Ok(repository) => Ok(repository.id),
            Err(error) if caller.is_none() && NotRegistered::refused(&error) => {
                let source = clone_source(origin).into();
                let cloned = self.register_repository(source, None, None).await?;
                Ok(cloned.id)
            }
            Err(error) => Err(error),
        }
    }
}

/// The source that registers a clone of `origin`: a local path as its
/// `file:` URL, since registering a path takes it as a checkout in place.
fn clone_source(origin: String) -> String {
    match Path::new(&origin).is_absolute() {
        true => format!("file://{origin}"),
        false => origin,
    }
}
