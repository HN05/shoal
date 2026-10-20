//! Tool caches every workspace of a repository shares, such as `GOCACHE` or
//! sccache's directory. Each lives in the repository directory's
//! `.caches/shared`,
//! and `[env]` names it with `{cache.NAME}`. Only caches whose tools lock or
//! address entries by content are safe to share between live workspaces.
use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};

use crate::{
    config::repo::Cache,
    fsutil::{create_marked_dir, is_marked_dir},
    model::Repository,
};

/// Holds the caches, apart from the owner marker.
const SHARED: &str = "shared";

pub fn validate(caches: &BTreeMap<String, Cache>) -> Result<()> {
    for name in caches.keys() {
        crate::validate::lowercase_name("cache", name)
            .with_context(|| format!("invalid configured cache name: {name}"))?;
    }
    Ok(())
}

/// The directory of one repository's shared caches.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub fn new(workspaces_dir: &Path, repository_id: &str) -> Self {
        Self {
            root: workspaces_dir.join(".caches"),
            owner: repository_id.to_owned(),
        }
    }

    /// The repository's caches; registrations from before the repository
    /// directory layout have none.
    pub fn of(repo: &Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    /// Each configured cache's directory, created when missing.
    pub fn prepare(&self, caches: &BTreeMap<String, Cache>) -> Result<BTreeMap<String, PathBuf>> {
        if caches.is_empty() {
            return Ok(BTreeMap::new());
        }
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        self.ensure_owned()?;
        caches
            .keys()
            .map(|name| {
                let directory = self.root.join(SHARED).join(name);
                fs::create_dir_all(&directory)
                    .with_context(|| format!("create {}", directory.display()))?;
                Ok((name.clone(), directory))
            })
            .collect()
    }

    /// Delete every cache with the directory itself, when the repository
    /// goes; a directory Shoal did not create is the user's and stays.
    pub fn remove(&self) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root)
    }

    fn ensure_owned(&self) -> Result<()> {
        ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a cache directory Shoal created; move it away to share caches",
            self.root.display()
        );
        Ok(())
    }
}

/// The directories of the repository's configured caches, created when missing.
pub fn directories(
    repo: &Repository,
    caches: &BTreeMap<String, Cache>,
) -> Result<BTreeMap<String, PathBuf>> {
    Layout::of(repo).map_or_else(|| Ok(BTreeMap::new()), |layout| layout.prepare(caches))
}

fn remove_dir(directory: &Path) -> Result<()> {
    match fs::remove_dir_all(directory) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("delete {}", directory.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::repo::CacheScope;

    fn configured(names: &[&str]) -> BTreeMap<String, Cache> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_owned(),
                    Cache {
                        scope: CacheScope::Shared,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn caches_are_created_and_removed_only_when_owned() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::new(root.path(), "repo");
        assert!(layout.prepare(&BTreeMap::new()).unwrap().is_empty());
        assert!(!root.path().join(".caches").exists());

        let directories = layout.prepare(&configured(&["go", "sccache"])).unwrap();
        assert_eq!(directories["go"], root.path().join(".caches/shared/go"));
        assert!(directories["sccache"].is_dir());

        // Another repository's directory is never used or deleted.
        let other = Layout::new(root.path(), "other");
        assert!(other.prepare(&configured(&["go"])).is_err());
        other.remove().unwrap();
        assert!(directories["go"].is_dir());

        layout.remove().unwrap();
        assert!(!root.path().join(".caches").exists());
    }

    #[test]
    fn cache_names_are_lowercase_identifiers() {
        validate(&configured(&["go", "pnpm_store", "gradle-home"])).unwrap();
        for name in ["", "Go", ".go", "../go", "go/build"] {
            assert!(validate(&configured(&[name])).is_err(), "{name:?}");
        }
    }
}
