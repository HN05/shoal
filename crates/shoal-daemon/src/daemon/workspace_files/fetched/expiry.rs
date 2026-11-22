//! How long fetched copies stay: `cleanup.restore.retention_days` after
//! their fetch, the longest their source stays once its space is removed,
//! or after their last change; or, when nobody changed them, until disk
//! space cleanup needs their space, since the machine that keeps the source
//! keeps them too; and every one goes with the repository it is kept beside.
use std::{path::Path, time::SystemTime};

use anyhow::{Result, ensure};
use shoal_ids::repository_ids::RepositoryId;

use super::copies::{remove_unchanged, remove_written_before};
use crate::{
    daemon::{log, manager::Manager},
    fsutil::off_runtime,
    model::Repository,
    shoal_dirs::{Contents, ShoalDir},
};

impl Manager {
    /// Delete the fetched copies fetched, or last changed, longer ago than
    /// the retention of the repository they are kept beside, or the global
    /// one in `root_dir`. A directory that fails keeps neither the others nor its
    /// copies for the next pass.
    pub(crate) async fn expire_fetched(&self) -> Result<()> {
        let mut failed = Vec::new();
        for home in self.fetched_homes().await? {
            let name = home.name.clone();
            if let Err(error) = self.expire_fetched_in(home).await {
                failed.push(format!("{name}: {error:#}"));
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    /// Delete the copies in `home` fetched, or last changed, longer ago than
    /// its retention.
    async fn expire_fetched_in(&self, home: Fetched) -> Result<()> {
        let retention = match &home.repository {
            Some(id) => self.repositories.repository_settings(id).await?,
            None => self.config().resolve(&Default::default())?,
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

    /// Delete every fetched copy where `on` selects that loses nothing, as
    /// on a filesystem low on space, returning whether any went: one a fetch
    /// is writing, or the user changed since, stays for its expiry. A failure is logged and keeps
    /// the rest of that directory.
    pub(crate) async fn delete_fetched_in(&self, on: impl Fn(&Path) -> bool) -> bool {
        let homes = match self.fetched_homes().await {
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

    /// Delete the directory of fetched copies beside the repository's
    /// workspaces, as it goes.
    pub(crate) async fn delete_repository_fetched(&self, repo: &Repository) -> Result<()> {
        let dir = ShoalDir::of(repo, Contents::Fetched);
        off_runtime::run(move || dir.remove()).await
    }

    /// Every directory of fetched copies: each registered repository's and
    /// `root_dir`'s.
    async fn fetched_homes(&self) -> Result<Vec<Fetched>> {
        let mut homes: Vec<_> = (self.repositories.list().await?.iter())
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

/// One directory of fetched copies, named for logs, with the repository
/// whose settings it follows; none for `root_dir`'s, which follows the
/// global ones.
struct Fetched {
    name: String,
    dir: ShoalDir,
    repository: Option<RepositoryId>,
}

#[cfg(test)]
mod tests;
