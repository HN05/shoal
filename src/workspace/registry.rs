//! Repository registration and lookup; independent of worktree lifecycle.
use super::Manager;
use crate::{git, model::Repository, repository, store, subprocess, validate};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;
use uuid::Uuid;

/// Directory name of a URL clone inside its repository directory.
pub const CHECKOUT_DIR: &str = "main";

impl Manager {
    pub async fn repositories(&self) -> Result<Vec<Repository>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT * FROM repositories ORDER BY last_used DESC")?
                    .query_map([], store::repository)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Register a local checkout in place, or clone a URL. Re-registering a
    /// known source returns (and optionally renames) the existing record.
    pub async fn register_repository(
        &self,
        source: String,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> Result<Repository> {
        if let Some(name) = &name {
            validate::name("repository", name)?;
        }
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
        let _guard = self.registry_gate.lock().await;
        let repositories = self.repositories().await?;
        if let Some(existing) = find_existing(&repositories, &source).await? {
            check_clone_path(existing, clone_path.as_deref())?;
            return match name {
                Some(name) => self.rename_repository(&existing.id, name).await,
                None => Ok(existing.clone()),
            };
        }
        let root = self.config.root_dir(&self.paths)?;
        let directory_name = name
            .clone()
            .unwrap_or_else(|| repository::directory_name(&source));
        let (path, workspaces_dir) = if PathBuf::from(&source).exists() {
            let root_dir = git::run(Path::new(&source), &["rev-parse", "--show-toplevel"]).await?;
            let path = fs::canonicalize(root_dir.trim())?;
            // A checkout already placed as `<root>/<x>/<checkout>` keeps that
            // directory, unless `<x>` is itself a checkout or another repository's.
            let placed = path
                .parent()
                .filter(|parent| parent.parent() == fs::canonicalize(&root).ok().as_deref())
                .filter(|parent| !parent.join(".git").exists())
                .filter(|parent| {
                    !repositories.iter().any(|repo| {
                        repo.path == *parent || repo.workspaces_dir.as_deref() == Some(parent)
                    })
                })
                .map(Path::to_path_buf);
            let workspaces_dir = match placed {
                Some(directory) => directory,
                None => reserve_directory(&root, &directory_name, &repositories)?,
            };
            (path, workspaces_dir)
        } else {
            ensure!(
                name.as_ref().is_none_or(|name| !repositories
                    .iter()
                    .any(|repo| repo.name.as_ref() == Some(name))),
                "repository name is already in use"
            );
            ensure!(
                source.contains("://") || source.contains('@'),
                "repository path does not exist: {source}"
            );
            let workspaces_dir = reserve_directory(&root, &directory_name, &repositories)?;
            let destination = match clone_path {
                Some(path) => create_clone_directory(path),
                None => create_clone_directory(workspaces_dir.join(CHECKOUT_DIR)),
            };
            let cloned = match destination {
                Ok(destination) => clone(&source, &destination).await,
                Err(error) => Err(error),
            };
            match cloned {
                Ok(path) => (path, workspaces_dir),
                Err(error) => {
                    // The reservation belongs exclusively to this attempt.
                    let _ = fs::remove_dir(&workspaces_dir);
                    return Err(error);
                }
            }
        };
        let repo = self
            .insert_repository(path, source, Some(workspaces_dir))
            .await?;
        match name {
            Some(name) => self.rename_repository(&repo.id, name).await,
            None => Ok(repo),
        }
    }

    async fn insert_repository(
        &self,
        path: PathBuf,
        source: String,
        workspaces_dir: Option<PathBuf>,
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
                    "INSERT INTO repositories (id,path,source,last_used,workspaces_dir) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories), ?4) ON CONFLICT(path) DO NOTHING",
                    params![id, path, source, workspaces_dir],
                )?;
                Ok(db.query_row(
                    "SELECT * FROM repositories WHERE path=?1",
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
        let root = self.config.root_dir(&self.paths)?;
        let directory = reserve_directory(
            &root,
            &repository::directory_name(repository::name(repo)),
            &repositories,
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

    pub async fn rename_repository(&self, selector: &str, name: String) -> Result<Repository> {
        validate::name("repository", &name)?;
        let repo = self.repository(selector).await?;
        self.store
            .run(move |db| {
                ensure!(
                    !db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM repositories WHERE name=?1 AND id<>?2)",
                        params![name, repo.id],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "repository name is already in use"
                );
                db.execute(
                    "UPDATE repositories SET name=?2 WHERE id=?1",
                    params![repo.id, name],
                )?;
                Ok(db.query_row(
                    "SELECT * FROM repositories WHERE id=?1",
                    [repo.id],
                    store::repository,
                )?)
            })
            .await
    }

    /// Resolve an ID, path, source URL, explicit name, derived name, or
    /// equivalent remote to a registered repository.
    pub(crate) async fn repository(&self, selector: &str) -> Result<Repository> {
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

async fn clone(source: &str, destination: &Path) -> Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["clone", "--"]).arg(source).arg(destination);
    if let Err(error) = subprocess::output(command).await {
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

/// Reserve `<root>/<name>` (suffixed `-2`, `-3`, ... when occupied or recorded)
/// by creating it, so concurrent daemons cannot share one directory.
fn reserve_directory(root: &Path, name: &str, repositories: &[Repository]) -> Result<PathBuf> {
    fs::create_dir_all(root)
        .with_context(|| format!("create repository root directory {}", root.display()))?;
    let root = fs::canonicalize(root)?;
    for suffix in 1_u64.. {
        let path = root.join(if suffix == 1 {
            name.to_owned()
        } else {
            format!("{name}-{suffix}")
        });
        // A missing directory still owns its recorded path.
        if repositories
            .iter()
            .any(|repo| repo.path == path || repo.workspaces_dir.as_deref() == Some(&*path))
        {
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
