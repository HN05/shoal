//! What the `xcode` preset leaves outside worktrees: each project's
//! DerivedData and the package checkouts in the preset's shared cache.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};

use super::Manager;
use crate::{
    config::Effective,
    daemon::log,
    git::{
        self,
        default_branch::{self, DefaultBranchLookup},
    },
    model::{Repository, Workspace},
    presets::{self, Preset, derived_data, xcode},
};

/// How long a package checkout nothing pins stays after a build last used it.
const CHECKOUTS_UNUSED_FOR: Duration = Duration::from_secs(24 * 60 * 60);

impl Manager {
    /// The workspace's settings when it uses the `xcode` preset, read while
    /// its worktree's own config is there to read. A missing worktree's
    /// settings come from its repository. Failing to read them keeps Xcode's
    /// data rather than the workspace.
    pub(super) async fn xcode_settings(
        &self,
        workspace: &Workspace,
        present: bool,
    ) -> Option<Effective> {
        let settings = if present {
            self.workspace_settings(workspace).await
        } else {
            self.repository_settings(&workspace.repository_id).await
        };
        match settings {
            Ok(settings) => uses_xcode(&settings).then_some(settings),
            Err(error) => {
                log!(
                    "Xcode data of {} kept: settings unreadable: {error:#}",
                    workspace.name
                );
                None
            }
        }
    }

    /// Delete what Xcode kept outside the removed worktree: the DerivedData
    /// of every project inside it, which Xcode rebuilds on demand, and
    /// package checkouts nothing pins any more.
    pub(super) async fn delete_xcode_data(&self, workspace: &Workspace, settings: &Effective) {
        if let Some(location) = derived_data::location(&self.paths.home) {
            let worktree = workspace.path.clone();
            delete_derived_data(&workspace.name, location, move |project| {
                project.starts_with(&worktree)
            })
            .await;
        }
        match self.repository(&workspace.repository_id).await {
            Ok(repo) => {
                if let Some(cache) = xcode_cache(&repo, settings) {
                    self.prune_checkouts(&repo, cache).await;
                }
            }
            Err(error) => log!("package checkouts of {} kept: {error:#}", workspace.name),
        }
    }

    /// Delete the Xcode data that removed workspaces of the repository left
    /// where `on` selects: DerivedData of projects whose worktree is gone,
    /// and package checkouts nothing pins any more. Returns whether anything
    /// was deleted.
    pub(crate) async fn delete_xcode_leftovers(
        &self,
        repo: &Repository,
        on: impl Fn(&Path) -> bool,
    ) -> bool {
        let name = crate::repository::name(repo);
        let settings = match self.repository_settings(&repo.id).await {
            Ok(settings) if uses_xcode(&settings) => settings,
            Ok(_) => return false,
            Err(error) => {
                log!("Xcode data of {name} kept: settings unreadable: {error:#}");
                return false;
            }
        };
        let mut deleted = false;
        if let Some(location) = derived_data::location(&self.paths.home)
            && on(&location)
        {
            deleted |= self.delete_left_derived_data(repo, location).await;
        }
        if let Some(cache) = xcode_cache(repo, &settings).filter(|cache| on(cache)) {
            deleted |= self.prune_checkouts(repo, cache).await;
        }
        deleted
    }

    /// Delete the DerivedData of projects whose worktree directory in the
    /// repository's directory is gone and no workspace record holds,
    /// returning whether any went. A project missing from a live worktree
    /// may be regenerating, and an absent repository directory, such as an
    /// unmounted volume, proves nothing gone. Creating, restoring and
    /// renaming a workspace hold the repository's Git gate, so holding it
    /// keeps a worktree from coming back meanwhile; landing holds it through
    /// a whole build, so a busy gate leaves the deletion to a later pass.
    async fn delete_left_derived_data(&self, repo: &Repository, location: PathBuf) -> bool {
        let name = crate::repository::name(repo);
        let Some(directory) = repo.workspaces_dir.clone() else {
            return false;
        };
        if !directory.try_exists().unwrap_or(false) {
            return false;
        }
        let Some(_git) = self.gates.try_lock_git(&repo.id) else {
            log!("DerivedData of {name} kept: the repository is busy");
            return false;
        };
        let recorded = match self.recorded_worktrees(repo).await {
            Ok(recorded) => recorded,
            Err(error) => {
                log!("DerivedData of {name} kept: {error:#}");
                return false;
            }
        };
        delete_derived_data(name, location, move |project| {
            worktree_gone(&directory, project)
                && !recorded
                    .iter()
                    .any(|worktree| project.starts_with(worktree))
        })
        .await
    }

    /// The recorded worktree paths of the repository's workspaces, whether
    /// or not they exist.
    async fn recorded_worktrees(&self, repo: &Repository) -> Result<Vec<PathBuf>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| workspace.repository_id == repo.id)
            .map(|workspace| workspace.path)
            .collect())
    }

    /// Delete the package checkouts in the preset's `cache` that nothing
    /// pins and no build used lately, returning whether any went. A workspace
    /// being created, restored or renamed may be about to use checkouts no
    /// worktree pins yet; each holds the repository's Git gate, and so does
    /// repository removal, which may be removing this workspace, so a busy
    /// gate leaves pruning to a later pass rather than waiting.
    async fn prune_checkouts(&self, repo: &Repository, cache: PathBuf) -> bool {
        let name = crate::repository::name(repo);
        let Some(_git) = self.gates.try_lock_git(&repo.id) else {
            log!("package checkouts of {name} kept: the repository is busy");
            return false;
        };
        let pruned = async {
            let pinned = self.pinned_checkouts(repo).await?;
            let unused_since = SystemTime::now() - CHECKOUTS_UNUSED_FOR;
            tokio::task::spawn_blocking(move || {
                xcode::prune_checkouts(&cache, &pinned, unused_since)
            })
            .await?
        }
        .await;
        match pruned {
            Ok(deleted) => {
                for checkouts in &deleted {
                    log!(
                        "deleted package checkouts {} of {name}",
                        checkouts.display()
                    );
                }
                !deleted.is_empty()
            }
            Err(error) => {
                log!("package checkouts of {name} kept: {error:#}");
                false
            }
        }
    }

    /// The keys of the package checkouts that the repository's checkout, a
    /// worktree of it or its default branch's tip pins.
    async fn pinned_checkouts(&self, repo: &Repository) -> Result<HashSet<String>> {
        let mut pins = Vec::new();
        for worktree in self.repository_worktrees(repo).await? {
            pins.extend(worktree_pins(&worktree).await?);
        }
        let branch = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let tip = git::files::at_revision(&repo.path, &branch, xcode::PINS).await?;
        pins.extend(tip.into_iter().map(String::into_bytes));
        Ok(pins.iter().map(|pins| xcode::checkouts_key(pins)).collect())
    }

    /// The repository's checkout and the worktrees of its workspaces that
    /// exist; one that cannot be inspected stops pruning.
    async fn repository_worktrees(&self, repo: &Repository) -> Result<Vec<PathBuf>> {
        let mut worktrees = vec![repo.path.clone()];
        for workspace in self.list_workspaces().await? {
            if workspace.repository_id == repo.id
                && workspace
                    .path
                    .try_exists()
                    .with_context(|| format!("inspect {}", workspace.path.display()))?
            {
                worktrees.push(workspace.path);
            }
        }
        Ok(worktrees)
    }
}

fn uses_xcode(settings: &Effective) -> bool {
    settings
        .presets
        .as_deref()
        .unwrap_or_default()
        .contains(&Preset::Xcode)
}

/// The preset's shared cache directory, when the preset configures it and
/// the repository owns the caches directory.
fn xcode_cache(repo: &Repository, settings: &Effective) -> Option<PathBuf> {
    if !settings.caches.contains_key(presets::XCODE_CACHE) {
        return None;
    }
    crate::caches::Layout::of(repo)?.owned(presets::XCODE_CACHE)
}

/// Whether the worktree directory holding `project` in the repository's
/// `directory` is gone.
fn worktree_gone(directory: &Path, project: &Path) -> bool {
    project
        .strip_prefix(directory)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|worktree| matches!(directory.join(worktree).try_exists(), Ok(false)))
}

/// The content of every `Package.resolved` in the worktree; one deleted
/// since the listing pins nothing, and one that cannot be read stops pruning.
async fn worktree_pins(worktree: &Path) -> Result<Vec<Vec<u8>>> {
    let mut pins = Vec::new();
    for file in git::files::in_worktree(worktree, xcode::PINS).await? {
        match std::fs::read(&file) {
            Ok(content) => pins.push(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("read {}", file.display())),
        }
    }
    Ok(pins)
}

/// Delete the DerivedData of every project `deletes` selects, logging what
/// went and returning whether any did; failure keeps it.
async fn delete_derived_data(
    owner: &str,
    location: PathBuf,
    deletes: impl Fn(&Path) -> bool + Send + 'static,
) -> bool {
    let deleted =
        tokio::task::spawn_blocking(move || derived_data::delete(&location, deletes)).await;
    match deleted
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        Ok(folders) => {
            for folder in &folders {
                log!("deleted DerivedData {} of {owner}", folder.display());
            }
            !folders.is_empty()
        }
        Err(error) => {
            log!("DerivedData of {owner} kept: {error:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests;
