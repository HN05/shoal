//! How long fetched copies stay. See `docs/design/coordination.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{path::Path, time::SystemTime};

use anyhow::{Result, ensure};
use shoal_ids::repository_ids::RepositoryId;

use super::copies::{remove_unchanged, remove_written_before};
use crate::{
    fsutil::off_runtime,
    log,
    model::Repository,
    shoal_dirs::{Contents, ShoalDir},
};

impl Spaces {
    /// Follows the retention of the repository they are kept beside, or the
    /// global one in `root_dir`, counted from the fetch or the last change.
    /// A directory that fails keeps neither the others nor its copies for
    /// the next pass.
    pub async fn expire_fetched(&self, daemon: &dyn Daemon) -> Result<()> {
        let mut failed = Vec::new();
        for home in self.fetched_homes(daemon).await? {
            let name = home.name.clone();
            if let Err(error) = self.expire_fetched_in(daemon, home).await {
                failed.push(format!("{name}: {error:#}"));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    async fn expire_fetched_in(&self, daemon: &dyn Daemon, home: Fetched) -> Result<()> {
        let retention = match &home.repository {
            Some(id) => daemon.repository_settings(id).await?,
            None => self.config.current().resolve(&Default::default())?,
        }
        .cleanup
        .restore
        .retention();
        let cutoff = SystemTime::now() - retention;
        let Fetched { name, dir, .. } = home;
        if off_runtime::run(move || remove_written_before(&dir, cutoff)).await? {
            log!("deleted the expired fetched copies of {name}");
        }
        Ok(())
    }

    /// Deletes only what loses nothing, as on a filesystem low on space: a
    /// copy a fetch is writing, or the user changed since, stays for its
    /// expiry. Returns whether any went. A failure is logged and keeps the
    /// rest of that directory.
    pub async fn delete_fetched_in(&self, daemon: &dyn Daemon, on: impl Fn(&Path) -> bool) -> bool {
        let homes = match self.fetched_homes(daemon).await {
            Ok(homes) => homes,
            Err(error) => {
                log!("fetched copies kept: {error:#}");
                return false;
            }
        };
        let mut deleted = false;
        for Fetched { name, dir, .. } in homes.into_iter().filter(|home| on(home.dir.root())) {
            match off_runtime::run(move || remove_unchanged(&dir)).await {
                Ok(removed) => deleted |= removed,
                Err(error) => log!("fetched copies of {name} kept: {error:#}"),
            }
        }
        deleted
    }

    /// Runs as the repository goes.
    pub async fn delete_repository_fetched(&self, repo: &Repository) -> Result<()> {
        let dir = ShoalDir::of(repo, Contents::Fetched);
        off_runtime::run(move || dir.remove()).await
    }

    /// Each registered repository's, and `root_dir`'s.
    async fn fetched_homes(&self, daemon: &dyn Daemon) -> Result<Vec<Fetched>> {
        let mut homes: Vec<_> = (daemon.repositories().await?.iter())
            .map(|repo| Fetched {
                name: repo.display_name().to_owned(),
                dir: ShoalDir::of(repo, Contents::Fetched),
                repository: Some(repo.id.clone()),
            })
            .collect();
        homes.push(Fetched {
            name: "root_dir".to_owned(),
            dir: self.fetched_outside_repositories()?,
            repository: None,
        });
        Ok(homes)
    }
}

/// Named for logs, with the repository whose settings it follows. None for
/// `root_dir`'s, which follows the global settings.
struct Fetched {
    name: String,
    dir: ShoalDir,
    repository: Option<RepositoryId>,
}

#[cfg(test)]
mod tests;
