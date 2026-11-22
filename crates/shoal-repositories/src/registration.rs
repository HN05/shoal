//! Registering a repository: a local checkout in place, or a clone of a URL
//! into a directory of its own, perhaps as a contribution.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use shoal_core::{config, git, repository};
use shoal_forge::repo::fork::ForkRequests;
use shoal_protocol::{model::Repository, validate};
use shoal_store as store;
use uuid::Uuid;

use crate::{
    Repositories,
    contributions::holds::ContributionHold,
    daemon::Daemon,
    root::{ConfiguredRoot, Recorded, prepare_root, reserve_directory},
};

/// Directory name of a URL clone inside its repository directory. Derived
/// workspace names never contain `.`, so no workspace can collide with it.
const CHECKOUT_DIR: &str = ".checkout";

/// What a registration is for.
#[derive(Clone, Copy)]
enum Registration<'a> {
    Regular,
    /// Removed once nothing of it is left, forking on the forge `daemon`
    /// reaches.
    Contribution(&'a dyn Daemon),
}

impl Repositories {
    /// Register a local checkout in place, or clone a URL. Re-registering a
    /// known source returns (and optionally renames) the existing record.
    pub async fn register(
        &self,
        source: repository::Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> Result<Repository> {
        let _guard = self.gates.lock_registry().await;
        (self.register_locked(source, name, clone_path, Registration::Regular)).await
    }

    /// Register a clone of `url`, which another machine of the hub has as
    /// `name`, under that name where no repository here has it and it is
    /// valid.
    pub async fn register_clone(&self, url: String, name: String) -> Result<Repository> {
        let _guard = self.gates.lock_registry().await;
        let repositories = self.list().await?;
        let taken = (repositories.iter()).any(|repo| repo.name.as_deref() == Some(&name));
        let name = (!taken && validate::name("repository", &name).is_ok()).then_some(name);
        (self.register_locked(url.into(), name, None, Registration::Regular)).await
    }

    /// `shoal contribute`: clone a repository URL as a contribution, which
    /// pushes to the user's fork when the user cannot push to the repository.
    /// An existing registration is returned as it is. The hold keeps it
    /// from cleanup from the moment it is registered.
    pub async fn register_contribution(
        &self,
        daemon: &dyn Daemon,
        source: String,
        name: Option<String>,
    ) -> Result<(Repository, ContributionHold)> {
        let _guard = self.gates.lock_registry().await;
        let registration = Registration::Contribution(daemon);
        let repo = (self.register_locked(source.into(), name, None, registration)).await?;
        let hold = self.contribution_holds.hold(&repo.id);
        Ok((repo, hold))
    }

    /// [`Self::register`] under the registry gate the caller holds.
    async fn register_locked(
        &self,
        source: repository::Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
        registration: Registration<'_>,
    ) -> Result<Repository> {
        if let Some(name) = &name {
            validate::name("repository", name)?;
        }
        let repositories = self.list().await?;
        let origins = repository::Origins::default();
        let source = match repository::select_with(&repositories, &source, &origins).await {
            Ok(existing)
                if existing.id == source.value()
                    || existing.source == source.value()
                    || existing.display_name() == source.value() =>
            {
                existing.source.clone()
            }
            Ok(_) => source.source()?.to_owned(),
            Err(error) if repository::NotRegistered::refused(&error) => source.source()?.to_owned(),
            Err(error) => return Err(error),
        };
        if let Some(path) = &clone_path {
            ensure!(path.is_absolute(), "repository clone path must be absolute");
            ensure!(
                !Path::new(&source).exists(),
                "--path is only for cloning URLs; local repositories are registered in place"
            );
        }
        // A trailing slash on a clone URL would become every worktree's origin
        // and break forge CLIs' repository detection; local paths canonicalize.
        let source = if Path::new(&source).exists() {
            source
        } else {
            source.trim_end_matches('/').to_owned()
        };
        if let Some(existing) = find_existing(&repositories, &source, &origins).await? {
            check_clone_path(existing, clone_path.as_deref())?;
            return match name {
                Some(name) => self.rename(&existing.id, name).await,
                None => Ok(existing.clone()),
            };
        }
        let root = self.configured_root()?;
        let state = fs::canonicalize(&self.paths.state)?;
        let directory_name = name
            .clone()
            .unwrap_or_else(|| repository::directory_name(&source));
        let (contribution, fork) = match registration {
            Registration::Regular => (false, None),
            Registration::Contribution(daemon) => (true, contribution_fork(daemon, &source).await?),
        };
        let workspaces = self.workspaces().await?;
        let recorded = Recorded {
            repositories: &repositories,
            workspaces: &workspaces,
        };
        let (path, workspaces_dir) = if Path::new(&source).exists() {
            local_checkout_location(&source, &root, &state, &directory_name, &recorded).await?
        } else {
            let cloning = Cloning {
                source: &source,
                fork: fork.as_deref(),
                path: clone_path,
            };
            clone_into_reserved_directory(
                cloning,
                &root,
                &state,
                &directory_name,
                name.as_deref(),
                &recorded,
            )
            .await?
        };
        let repo = self
            .insert(path, source, workspaces_dir, contribution)
            .await?;
        match name {
            Some(name) => self.rename(&repo.id, name).await,
            None => Ok(repo),
        }
    }

    async fn insert(
        &self,
        path: PathBuf,
        source: String,
        workspaces_dir: PathBuf,
        contribution: bool,
    ) -> Result<Repository> {
        let path = path
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned();
        let workspaces_dir = workspaces_dir
            .to_str()
            .context("repository directory is not UTF-8")?
            .to_owned();
        let id = Uuid::new_v4().to_string();
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO repositories (id,path,source,last_used,workspaces_dir,contribution) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories), ?4, ?5) ON CONFLICT(path) DO NOTHING",
                    params![id, path, source, workspaces_dir, contribution],
                )?;
                Ok(db.query_row(
                    &format!("SELECT {} FROM repositories WHERE path=?1", store::REPOSITORY_COLUMNS),
                    [path],
                    store::repository,
                )?)
            })
            .await
    }
}

/// The URL of the user's fork of `source` to push to, created when the
/// user cannot push to `source` itself.
async fn contribution_fork(daemon: &dyn Daemon, source: &str) -> Result<Option<String>> {
    ensure!(
        !Path::new(source).exists(),
        "a contribution clones a repository URL; {source} is a local path"
    );
    let forge = daemon.forge(source).await?;
    let account = daemon.user_account();
    if forge.pushable(&account).await? {
        return Ok(None);
    }
    let fork = forge.fork(&account).await?;
    fork.remote_like(source).map(Some)
}

/// A registration with the same source string or the same remote identity,
/// reading the checkouts' origins through `origins`.
async fn find_existing<'a>(
    repositories: &'a [Repository],
    source: &str,
    origins: &repository::Origins,
) -> Result<Option<&'a Repository>> {
    if let Some(repo) = repositories.iter().find(|r| r.source == source) {
        return Ok(Some(repo));
    }
    repository::find_by_identity(repositories, source, origins).await
}

/// Locate a local checkout and retain its existing repository directory when
/// it is already placed under the configured root.
async fn local_checkout_location(
    source: &str,
    root: &ConfiguredRoot,
    state: &Path,
    directory_name: &str,
    recorded: &Recorded<'_>,
) -> Result<(PathBuf, PathBuf)> {
    let path = git::worktree_root(Path::new(source)).await?;
    let root = prepare_root(root, state, recorded, Some(&path)).await?;
    let repositories = recorded.repositories;
    outside_local_configs(&path, repositories)?;
    // A checkout already placed as `<root>/<x>/<checkout>` keeps that
    // directory, unless `<x>` is itself a checkout or another repository's,
    // or the checkout sits where `<x>`'s local config belongs.
    let placed = path
        .parent()
        .filter(|_| path.file_name() != Some(std::ffi::OsStr::new(config::repo::LOCAL_DIR)))
        .filter(|parent| parent.parent() == Some(root.as_path()))
        .filter(|parent| !parent.join(".git").exists())
        .filter(|parent| {
            !repositories
                .iter()
                .any(|repo| repo.path == *parent || repo.workspaces_dir == *parent)
        })
        .map(Path::to_path_buf);
    let workspaces_dir = match placed {
        Some(directory) => directory,
        None => reserve_directory(&root, directory_name, repositories, &[])?,
    };
    Ok((path, workspaces_dir))
}

/// A checkout at `path` would not lie where a repository's local config
/// belongs.
fn outside_local_configs(path: &Path, repositories: &[Repository]) -> Result<()> {
    for directory in repositories
        .iter()
        .map(|repo| repo.workspaces_dir.as_path())
    {
        let local = directory.join(config::repo::LOCAL_DIR);
        ensure!(
            !path.starts_with(&local),
            "{} is where another repository's local config belongs; register a checkout elsewhere",
            path.display()
        );
    }
    Ok(())
}

/// The URL a registration clones, the fork it pushes to, and an exact
/// directory requested for the clone.
struct Cloning<'a> {
    source: &'a str,
    fork: Option<&'a str>,
    path: Option<PathBuf>,
}

/// Reserve a repository directory, clone into it, and release the reservation
/// when any part of the clone attempt fails.
async fn clone_into_reserved_directory(
    cloning: Cloning<'_>,
    root: &ConfiguredRoot,
    state: &Path,
    directory_name: &str,
    requested_name: Option<&str>,
    recorded: &Recorded<'_>,
) -> Result<(PathBuf, PathBuf)> {
    let repositories = recorded.repositories;
    ensure!(
        requested_name.is_none_or(|name| {
            repositories
                .iter()
                .all(|repo| repo.name.as_deref() != Some(name))
        }),
        "repository name is already in use"
    );
    let source = cloning.source;
    ensure!(
        source.contains("://") || source.contains('@'),
        "repository path does not exist: {source}"
    );
    let root = prepare_root(root, state, recorded, None).await?;
    let requested = requested_clone_path(cloning.path.as_deref(), repositories).await?;
    let workspaces_dir =
        reserve_clone_directory(&root, directory_name, repositories, requested.as_deref())?;
    let destination = match cloning.path {
        Some(path) => create_clone_directory(path),
        None => create_clone_directory(workspaces_dir.join(CHECKOUT_DIR)),
    };
    let cloned = match destination {
        Ok(destination) => clone(source, cloning.fork, &destination).await,
        Err(error) => Err(error),
    };
    match cloned {
        Ok(path) => Ok((path, workspaces_dir)),
        Err(error) => {
            // The reservation belongs exclusively to this attempt.
            let _ = fs::remove_dir(&workspaces_dir);
            Err(error)
        }
    }
}

/// The canonical form of an explicit clone path, which the daemon can write
/// and no repository's local config holds.
async fn requested_clone_path(
    path: Option<&Path>,
    repositories: &[Repository],
) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    if let Some(parent) = path.parent() {
        shoal_daemon_base::writable::ensure_writable(parent).await?;
    }
    let path = shoal_daemon_base::ownership::paths::canonical_with_missing_tail(path)?;
    outside_local_configs(&path, repositories)?;
    Ok(Some(path))
}

/// Reserve the repository directory for a clone to `requested`, or into the
/// directory itself, releasing it when the clone would land where its local
/// config belongs.
fn reserve_clone_directory(
    root: &Path,
    name: &str,
    repositories: &[Repository],
    requested: Option<&Path>,
) -> Result<PathBuf> {
    let workspaces_dir = reserve_directory(root, name, repositories, &[])?;
    let local = workspaces_dir.join(config::repo::LOCAL_DIR);
    if let Some(path) = requested.filter(|path| path.starts_with(&local)) {
        // The reservation belongs exclusively to this attempt.
        let _ = fs::remove_dir(&workspaces_dir);
        bail!(
            "{} is where the repository's local config belongs; clone it elsewhere",
            path.display()
        );
    }
    Ok(workspaces_dir)
}

async fn clone(source: &str, fork: Option<&str>, destination: &Path) -> Result<PathBuf> {
    if let Err(error) = git::clone(source, destination, fork).await {
        // This directory belongs exclusively to this attempt.
        if destination.exists() {
            fs::remove_dir_all(destination).context("clean up failed repository clone")?;
        }
        return Err(error);
    }
    Ok(fs::canonicalize(destination)?)
}

fn create_clone_directory(path: PathBuf) -> Result<PathBuf> {
    let directory = path
        .parent()
        .context("clone path must name a new directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create repository parent directory {}", directory.display()))?;
    fs::create_dir(&path).with_context(|| {
        format!(
            "clone destination must not already exist: {}",
            path.display()
        )
    })?;
    Ok(path)
}

fn check_clone_path(repo: &Repository, requested: Option<&Path>) -> Result<()> {
    if let Some(path) = requested {
        ensure!(
            fs::canonicalize(path).is_ok_and(|path| path == repo.path),
            "repository is already registered at {}; --path cannot relocate it",
            repo.path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
