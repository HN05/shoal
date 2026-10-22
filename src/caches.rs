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
/// Where emptied caches wait for deletion, inside `.caches` so moving one
/// there never crosses filesystems.
const TRASH: &str = "trash";

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Each configured cache's directory, without creating it.
    pub fn locate(&self, caches: &BTreeMap<String, Cache>) -> BTreeMap<String, PathBuf> {
        caches
            .keys()
            .map(|name| (name.clone(), self.root.join(SHARED).join(name)))
            .collect()
    }

    /// The directory of the cache `name` when the caches directory is the
    /// one Shoal created for this repository and no symlink leads out of it
    /// on the way, so its entries may be deleted.
    pub fn owned(&self, name: &str) -> Option<PathBuf> {
        let shared = self.root.join(SHARED);
        let cache = shared.join(name);
        let real = |directory: &Path| {
            fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.is_dir())
        };
        (is_marked_dir(&self.root, &self.owner) && real(&shared) && real(&cache)).then_some(cache)
    }

    /// Each configured cache's directory, created when missing.
    pub fn prepare(&self, caches: &BTreeMap<String, Cache>) -> Result<BTreeMap<String, PathBuf>> {
        if caches.is_empty() {
            return Ok(BTreeMap::new());
        }
        create_marked_dir(&self.root, &self.owner)
            .with_context(|| format!("create {}", self.root.display()))?;
        self.ensure_owned()?;
        let directories = self.locate(caches);
        for (name, directory) in &directories {
            fs::create_dir_all(directory)
                .with_context(|| format!("create {}", directory.display()))?;
            crate::presets::prepare_cache(name, directory)?;
        }
        Ok(directories)
    }

    /// The caches that exist, configured or not.
    pub fn list(&self) -> Result<Vec<String>> {
        let shared = self.root.join(SHARED);
        let entries = match fs::read_dir(&shared) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("read {}", shared.display())),
        };
        let mut names = entries
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }

    /// Move every cache aside for [`Self::purge`] and return the names moved;
    /// the next process Shoal starts recreates the directories it uses.
    /// Callers keep tracked executions from starting meanwhile.
    pub fn move_aside(&self) -> Result<Vec<String>> {
        let names = self.list()?;
        if names.is_empty() {
            return Ok(names);
        }
        self.ensure_owned()?;
        let trash = self.root.join(TRASH);
        fs::create_dir_all(&trash)?;
        let shared = self.root.join(SHARED);
        fs::rename(&shared, trash.join(uuid::Uuid::new_v4().to_string()))
            .with_context(|| format!("move {} aside", shared.display()))?;
        Ok(names)
    }

    /// Delete the caches [`Self::move_aside`] moved, including any an
    /// interrupted pass left behind; a directory Shoal did not create stays.
    pub fn purge(&self) -> Result<()> {
        if !is_marked_dir(&self.root, &self.owner) {
            return Ok(());
        }
        remove_dir(&self.root.join(TRASH))
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

/// The directories of the repository's configured caches, without creating them.
pub fn locate(repo: &Repository, caches: &BTreeMap<String, Cache>) -> BTreeMap<String, PathBuf> {
    Layout::of(repo).map_or_else(BTreeMap::new, |layout| layout.locate(caches))
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

    #[test]
    fn only_a_cache_directory_marked_for_the_repository_is_owned() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::new(root.path(), "repo");
        fs::create_dir(layout.root()).unwrap();
        assert_eq!(layout.owned("go"), None, "unmarked");
        layout.remove().unwrap();
        fs::remove_dir(layout.root()).unwrap();
        Layout::new(root.path(), "other")
            .prepare(&configured(&["go"]))
            .unwrap();
        assert_eq!(layout.owned("go"), None, "another repository's");
        Layout::new(root.path(), "other").remove().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        layout.prepare(&configured(&["go"])).unwrap();
        let shared = root.path().join(".caches/shared");
        assert_eq!(layout.owned("go"), Some(shared.join("go")));
        std::os::unix::fs::symlink(&outside, shared.join("linked")).unwrap();
        assert_eq!(layout.owned("linked"), None, "a symlinked cache");
        fs::rename(&shared, root.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(root.path().join("moved"), &shared).unwrap();
        assert_eq!(layout.owned("go"), None, "a symlinked shared directory");
    }

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
    fn caches_are_created_emptied_and_removed_only_when_owned() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::new(root.path(), "repo");
        let directory = root.path().join(".caches");
        assert!(layout.prepare(&BTreeMap::new()).unwrap().is_empty());
        assert!(!directory.exists());

        let directories = layout.prepare(&configured(&["go", "sccache"])).unwrap();
        assert_eq!(directories["go"], root.path().join(".caches/shared/go"));
        fs::write(directories["go"].join("entry"), "cached").unwrap();
        let dropped = directory.join(SHARED).join("dropped");
        fs::create_dir(&dropped).unwrap();

        assert_eq!(layout.list().unwrap(), ["dropped", "go", "sccache"]);
        let moved = layout.move_aside().unwrap();
        assert_eq!(moved, ["dropped", "go", "sccache"]);
        assert!(layout.list().unwrap().is_empty());
        assert!(!dropped.exists());
        assert!(directory.join(TRASH).is_dir());
        layout.purge().unwrap();
        assert!(!directory.join(TRASH).exists());
        let directories = layout.prepare(&configured(&["go", "sccache"])).unwrap();

        // Another repository's directory is never used or deleted.
        let other = Layout::new(root.path(), "other");
        assert!(other.prepare(&configured(&["go"])).is_err());
        assert!(other.move_aside().is_err());
        other.remove().unwrap();
        assert!(directories["sccache"].is_dir());

        layout.remove().unwrap();
        assert!(!directory.exists());
        assert!(layout.move_aside().unwrap().is_empty());
    }

    #[test]
    fn cache_names_are_lowercase_identifiers() {
        validate(&configured(&["go", "pnpm_store", "gradle-home"])).unwrap();
        for name in ["", "Go", ".go", "../go", "go/build"] {
            assert!(validate(&configured(&[name])).is_err(), "{name:?}");
        }
    }
}
