//! Repository registration and lookup; independent of worktree lifecycle.
use super::{Manager, spotlight};
use crate::{
    daemon::store,
    git,
    model::{Repository, Workspace},
    repository, validate,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Directory name of a URL clone inside its repository directory. Derived
/// workspace names never contain `.`, so no workspace can collide with it.
pub const CHECKOUT_DIR: &str = ".checkout";

/// What a registration is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Registration {
    Regular,
    /// Removed once nothing of it is left.
    Contribution,
}

impl Manager {
    pub async fn repositories(&self) -> Result<Vec<Repository>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare(&format!(
                        "SELECT {} FROM repositories ORDER BY last_used DESC",
                        store::REPOSITORY_COLUMNS
                    ))?
                    .query_map([], store::repository)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Register a local checkout in place, or clone a URL. Re-registering a
    /// known source returns (and optionally renames) the existing record.
    pub async fn register_repository(
        &self,
        source: repository::Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> Result<Repository> {
        self.register(source, name, clone_path, Registration::Regular)
            .await
    }

    /// `shoal contribute`: clone a repository URL as a contribution, which
    /// pushes to the user's fork when the user cannot push to the repository.
    /// An existing registration is returned as it is.
    pub async fn register_contribution(
        &self,
        source: String,
        name: Option<String>,
    ) -> Result<Repository> {
        self.register(source.into(), name, None, Registration::Contribution)
            .await
    }

    async fn register(
        &self,
        source: repository::Selector,
        name: Option<String>,
        clone_path: Option<PathBuf>,
        registration: Registration,
    ) -> Result<Repository> {
        if let Some(name) = &name {
            validate::name("repository", name)?;
        }
        let _guard = self.gates.lock_registry().await;
        let repositories = self.repositories().await?;
        let source = match repository::select(&repositories, &source).await {
            Ok(existing)
                if existing.id == source.value()
                    || existing.source == source.value()
                    || repository::name(existing) == source.value() =>
            {
                existing.source.clone()
            }
            Ok(_) => source.source()?.to_owned(),
            Err(error) if error.is::<repository::NotRegistered>() => source.source()?.to_owned(),
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
        if let Some(existing) = find_existing(&repositories, &source).await? {
            check_clone_path(existing, clone_path.as_deref())?;
            return match name {
                Some(name) => self.rename_repository(&existing.id, name).await,
                None => Ok(existing.clone()),
            };
        }
        let root = self.configured_root()?;
        let state = fs::canonicalize(&self.paths.state)?;
        let directory_name = name
            .clone()
            .unwrap_or_else(|| repository::directory_name(&source));
        let contribution = registration == Registration::Contribution;
        let fork = match registration {
            Registration::Regular => None,
            Registration::Contribution => self.contribution_fork(&source).await?,
        };
        let workspaces = self.list_workspaces().await?;
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
            .insert_repository(path, source, Some(workspaces_dir), contribution)
            .await?;
        match name {
            Some(name) => self.rename_repository(&repo.id, name).await,
            None => Ok(repo),
        }
    }

    /// The URL of the user's fork of `source` to push to, created when the
    /// user cannot push to `source` itself.
    async fn contribution_fork(&self, source: &str) -> Result<Option<String>> {
        ensure!(
            !Path::new(source).exists(),
            "a contribution clones a repository URL; {source} is a local path"
        );
        let forge = self.forge(source).await?;
        let account = self.user_account();
        if forge.pushable(&account).await? {
            return Ok(None);
        }
        let fork = forge.fork(&account).await?;
        fork.remote_like(source).map(Some)
    }

    async fn insert_repository(
        &self,
        path: PathBuf,
        source: String,
        workspaces_dir: Option<PathBuf>,
        contribution: bool,
    ) -> Result<Repository> {
        let path = path
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned();
        let workspaces_dir = workspaces_dir
            .map(|dir| {
                dir.to_str()
                    .map(str::to_owned)
                    .context("repository directory is not UTF-8")
            })
            .transpose()?;
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

    /// The repository's directory under `root_dir`, reserved on first use for
    /// registrations that predate it. Callers hold the repository's Git gate,
    /// which serializes this with repository removal; the registry gate is not
    /// taken here because removal acquires the two in the opposite order, and
    /// `reserve_directory` is atomic against concurrent registrations anyway.
    pub(crate) async fn workspaces_dir(&self, repo: &Repository) -> Result<PathBuf> {
        if let Some(directory) = &repo.workspaces_dir {
            return Ok(directory.clone());
        }
        let repositories = self.repositories().await?;
        let current = repositories
            .iter()
            .find(|other| other.id == repo.id)
            .context("repository was removed")?;
        if let Some(directory) = &current.workspaces_dir {
            return Ok(directory.clone());
        }
        let workspaces = self.list_workspaces().await?;
        let recorded = Recorded {
            repositories: &repositories,
            workspaces: &workspaces,
        };
        let root = prepare_root(
            &self.configured_root()?,
            &fs::canonicalize(&self.paths.state)?,
            &recorded,
            None,
        )
        .await?;
        let directory = reserve_directory(
            &root,
            &repository::directory_name(repository::name(repo)),
            &repositories,
            &[],
        )?;
        let id = repo.id.clone();
        let recorded = directory
            .to_str()
            .context("repository directory is not UTF-8")?
            .to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE repositories SET workspaces_dir=?2 WHERE id=?1",
                    params![id, recorded],
                )?;
                Ok(())
            })
            .await?;
        Ok(directory)
    }

    pub async fn rename_repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
        name: String,
    ) -> Result<Repository> {
        validate::name("repository", &name)?;
        let repo = self.repository(selector).await?;
        self.store
            .run(move |db| {
                ensure!(
                    !store::exists(
                        db,
                        "SELECT 1 FROM repositories WHERE name=?1 AND id<>?2",
                        params![name, repo.id]
                    )?,
                    "repository name is already in use"
                );
                db.execute(
                    "UPDATE repositories SET name=?2 WHERE id=?1",
                    params![repo.id, name],
                )?;
                Ok(db.query_row(
                    &format!(
                        "SELECT {} FROM repositories WHERE id=?1",
                        store::REPOSITORY_COLUMNS
                    ),
                    [repo.id],
                    store::repository,
                )?)
            })
            .await
    }

    /// Resolve an ID, path, source URL, explicit name, derived name, or
    /// equivalent remote to a registered repository.
    pub(crate) async fn repository(
        &self,
        selector: impl Into<crate::repository::Selector>,
    ) -> Result<Repository> {
        Ok(repository::select(&self.repositories().await?, selector)
            .await?
            .clone())
    }
}

/// A registration with the same source string or the same remote identity.
async fn find_existing<'a>(
    repositories: &'a [Repository],
    source: &str,
) -> Result<Option<&'a Repository>> {
    if let Some(repo) = repositories.iter().find(|r| r.source == source) {
        return Ok(Some(repo));
    }
    repository::find_by_identity(repositories, source).await
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
    let toplevel = git::worktree_root(Path::new(source)).await?;
    let path = fs::canonicalize(toplevel.trim())?;
    let root = prepare_root(root, state, recorded, Some(&path)).await?;
    let repositories = recorded.repositories;
    outside_local_configs(&path, repositories)?;
    // A checkout already placed as `<root>/<x>/<checkout>` keeps that
    // directory, unless `<x>` is itself a checkout or another repository's,
    // or the checkout sits where `<x>`'s local config belongs.
    let placed = path
        .parent()
        .filter(|_| path.file_name() != Some(std::ffi::OsStr::new(crate::config::repo::LOCAL_DIR)))
        .filter(|parent| parent.parent() == Some(root.as_path()))
        .filter(|parent| !parent.join(".git").exists())
        .filter(|parent| {
            !repositories
                .iter()
                .any(|repo| repo.path == *parent || repo.workspaces_dir.as_deref() == Some(parent))
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
        .filter_map(|repo| repo.workspaces_dir.as_deref())
    {
        let local = directory.join(crate::config::repo::LOCAL_DIR);
        ensure!(
            !path.starts_with(&local),
            "{} is where another repository's local config belongs; register a checkout elsewhere",
            path.display()
        );
    }
    Ok(())
}

/// Reserve a repository directory, clone into it, and release the reservation
/// when any part of the clone attempt fails.
/// The URL a registration clones, the fork it pushes to, and an exact
/// directory requested for the clone.
struct Cloning<'a> {
    source: &'a str,
    fork: Option<&'a str>,
    path: Option<PathBuf>,
}

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
    if let Some(parent) = cloning.path.as_deref().and_then(Path::parent) {
        crate::daemon::access::ensure_writable(parent).await?;
    }
    let requested = cloning
        .path
        .as_deref()
        .map(super::paths::canonical_with_missing_tail)
        .transpose()?;
    if let Some(path) = &requested {
        outside_local_configs(path, repositories)?;
    }
    let workspaces_dir = reserve_directory(&root, directory_name, repositories, &[])?;
    let local = workspaces_dir.join(crate::config::repo::LOCAL_DIR);
    if let Some(path) = requested.filter(|path| path.starts_with(&local)) {
        // The reservation belongs exclusively to this attempt.
        let _ = fs::remove_dir(&workspaces_dir);
        bail!(
            "{} is where the repository's local config belongs; clone it elsewhere",
            path.display()
        );
    }
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

/// The configured `root_dir` and how preparing it treats it.
pub(super) struct ConfiguredRoot {
    pub(super) path: PathBuf,
    pub(super) layout: spotlight::Layout,
}

impl Manager {
    /// The configured `root_dir`, taken as registration finds it.
    pub(super) fn configured_root(&self) -> Result<ConfiguredRoot> {
        Ok(ConfiguredRoot {
            path: self.config().root_dir(&self.paths)?,
            layout: spotlight::Layout::AsFound,
        })
    }
}

/// What Shoal records, which a root must stay out of.
pub(super) struct Recorded<'a> {
    pub(super) repositories: &'a [Repository],
    pub(super) workspaces: &'a [Workspace],
}

/// The canonical, existing directory that holds repository directories: the
/// `root_dir`, or where its layout keeps them. Worktrees must stay out of
/// Shoal's state, every checkout, including the one `checkout` registers,
/// every repository directory and every workspace, and the daemon must be
/// able to write there, so a root it cannot write or that is or resolves
/// inside one of them is refused before anything is created or linked.
pub(super) async fn prepare_root(
    root: &ConfiguredRoot,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<PathBuf> {
    let plan = plan_root(root, state, recorded, checkout).await?;
    plan.apply()?;
    plan.destination()
}

/// What [`prepare_root`] would change, refused as it would refuse, without
/// changing anything.
pub(super) async fn plan_root(
    root: &ConfiguredRoot,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<spotlight::Plan> {
    let plan = spotlight::plan(&root.path, root.layout)?;
    for location in plan.locations()? {
        ensure_outside(&location, state, recorded, checkout)?;
        crate::daemon::access::ensure_writable(&location).await?;
    }
    Ok(plan)
}

/// Refuse a root `location` inside Shoal's state, a checkout, a repository
/// directory or a workspace.
fn ensure_outside(
    location: &Path,
    state: &Path,
    recorded: &Recorded<'_>,
    checkout: Option<&Path>,
) -> Result<()> {
    let repositories = recorded.repositories;
    ensure!(
        !location.starts_with(state),
        "root_dir {} is inside Shoal's state directory",
        location.display()
    );
    let checkouts = repositories.iter().map(|repo| repo.path.as_path());
    for checkout in checkouts.chain(checkout) {
        ensure!(
            !location.starts_with(checkout),
            "root_dir {} is inside the repository checkout {}",
            location.display(),
            checkout.display()
        );
    }
    for directory in repositories
        .iter()
        .filter_map(|repo| repo.workspaces_dir.as_deref())
    {
        ensure!(
            !location.starts_with(directory),
            "root_dir {} is inside the repository directory {}",
            location.display(),
            directory.display()
        );
    }
    if let Some(workspace) = recorded
        .workspaces
        .iter()
        .find(|workspace| location.starts_with(&workspace.path))
    {
        bail!(
            "root_dir {} is inside the workspace {}",
            location.display(),
            workspace.name
        );
    }
    Ok(())
}

/// Reserve `<root>/<name>` (suffixed `-2`, `-3`, ... when occupied or recorded)
/// by creating it, so concurrent daemons cannot share one directory. A
/// candidate that is, holds or lies in a recorded checkout, repository
/// directory or one of `workspaces` is taken, even when that is gone.
pub(super) fn reserve_directory(
    root: &Path,
    name: &str,
    repositories: &[Repository],
    workspaces: &[PathBuf],
) -> Result<PathBuf> {
    let recorded = || {
        repositories
            .iter()
            .flat_map(|repo| std::iter::once(&repo.path).chain(repo.workspaces_dir.as_ref()))
            .chain(workspaces)
    };
    // Every candidate would lie in it, whatever the suffix.
    if let Some(owner) = recorded().find(|owned| root.starts_with(owned)) {
        bail!(
            "root_dir {} is inside {}, which Shoal already records",
            root.display(),
            owner.display()
        );
    }
    for suffix in 1_u64.. {
        let path = root.join(if suffix == 1 {
            name.to_owned()
        } else {
            format!("{name}-{suffix}")
        });
        // A missing directory still owns its recorded path.
        if recorded().any(|owned| owned.starts_with(&path) || path.starts_with(owned)) {
            continue;
        }
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reserve repository directory {}", path.display()));
            }
        }
    }
    bail!("repository directory suffixes exhausted")
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
