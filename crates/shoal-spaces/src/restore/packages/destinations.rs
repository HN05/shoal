//! The repositories a package's spaces go to: the registered one the import
//! names for its own space, else the one with each space's origin, which
//! the import registers as a clone of that origin when none here has it.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};

use crate::scope::Caller;
use crate::{model::RemovedSpace, repository::NotRegistered, transfer::Manifest};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::repository::Selector;

/// By the ID it had there.
#[derive(Default)]
pub(super) struct Destinations {
    /// None for a chat without a repository.
    own: Option<RepositoryId>,
    here: BTreeMap<RepositoryId, RepositoryId>,
}

impl Destinations {
    pub(super) fn own(&self) -> Option<&RepositoryId> {
        self.own.as_ref()
    }

    /// None for a chat without a repository.
    pub(super) fn of(&self, space: &RemovedSpace) -> Option<Selector> {
        let there = space.repository_id.as_ref()?;
        self.here.get(there).map(|here| here.clone().into())
    }
}

impl Spaces {
    /// The one `repository` names for its own space and every space of the
    /// same repository, else the one with the origin each space records. A
    /// scoped `caller` imports every space only into its own repository and
    /// clones none.
    pub(super) async fn destinations(
        &self,
        daemon: &dyn Daemon,
        manifest: &Manifest,
        repository: Option<&Selector>,
        caller: Option<&Caller>,
    ) -> Result<Destinations> {
        let mut destinations = Destinations::default();
        if let Some(there) = &manifest.space.record.repository_id {
            let here = match repository {
                Some(named) => {
                    daemon
                        .repository(crate::repository::Selector::from(named))
                        .await?
                        .id
                }
                None => self.own_destination(daemon, manifest, caller).await?,
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
                self.with_origin(daemon, origin, caller).await
            };
            let here = (here.await).with_context(|| format!("{name} goes with it, but"))?;
            destinations.here.insert(there.clone(), here);
        }
        Ok(destinations)
    }

    /// As [`Self::with_origin`] finds it for `caller`.
    async fn own_destination(
        &self,
        daemon: &dyn Daemon,
        manifest: &Manifest,
        caller: Option<&Caller>,
    ) -> Result<RepositoryId> {
        let origin = (manifest.remote_url.clone()).context(
            "the exported repository has no origin; select the repository to import into with --repo",
        )?;
        self.with_origin(daemon, origin, caller).await
    }

    /// The user's import registers a clone of it when none here has it. A
    /// scoped `caller` imports only into its own repository and clones none.
    async fn with_origin(
        &self,
        daemon: &dyn Daemon,
        origin: String,
        caller: Option<&Caller>,
    ) -> Result<RepositoryId> {
        let selector = crate::repository::named(origin.clone())?;
        if let Some(caller) = caller {
            const REFUSAL: &str = "scoped processes import only into their own repository";
            let own = daemon
                .coordination()
                .own_repository(daemon, caller, &selector, REFUSAL)
                .await?;
            return Ok(daemon
                .repository(crate::repository::Selector::from(&own))
                .await?
                .id);
        }
        match daemon.repository(selector).await {
            Ok(repository) => Ok(repository.id),
            Err(error) if NotRegistered::refused(&error) => {
                let source = clone_source(origin).into();
                let cloned = daemon.register_repository(source, None, None).await?;
                Ok(cloned.id)
            }
            Err(error) => Err(error),
        }
    }
}

/// A local path goes as its `file:` URL, since registering a path takes it
/// as a checkout in place.
fn clone_source(origin: String) -> String {
    match Path::new(&origin).is_absolute() {
        true => format!("file://{origin}"),
        false => origin,
    }
}
