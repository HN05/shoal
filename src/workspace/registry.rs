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
        let _guard = self.registry_gate.lock().await;
        let repositories = self.repositories().await?;
        if let Some(existing) = find_existing(&repositories, &source).await? {
            check_clone_path(existing, clone_path.as_deref())?;
            return match name {
                Some(name) => self.rename_repository(&existing.id, name).await,
                None => Ok(existing.clone()),
            };
        }
        let path = if PathBuf::from(&source).exists() {
            let root = git::run(Path::new(&source), &["rev-parse", "--show-toplevel"]).await?;
            fs::canonicalize(root.trim())?
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
            let destination = match clone_path {
                Some(path) => create_clone_directory(path)?,
                None => {
                    let directory = self.config.repositories_dir(&self.paths)?;
                    let directory_name = name
                        .clone()
                        .unwrap_or_else(|| repository::directory_name(&source));
                    reserve_clone_directory(&directory, &directory_name, &repositories)?
                }
            };
            clone(&source, &destination).await?
        };
        let repo = self.insert_repository(path, source).await?;
        match name {
            Some(name) => self.rename_repository(&repo.id, name).await,
            None => Ok(repo),
        }
    }

    async fn insert_repository(&self, path: PathBuf, source: String) -> Result<Repository> {
        let path = path
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned();
        let id = Uuid::new_v4().to_string();
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO repositories (id,path,source,last_used) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories)) ON CONFLICT(path) DO NOTHING",
                    params![id, path, source],
                )?;
                Ok(db.query_row(
                    "SELECT * FROM repositories WHERE path=?1",
                    [path],
                    store::repository,
                )?)
            })
            .await
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

fn reserve_clone_directory(
    directory: &Path,
    name: &str,
    repositories: &[Repository],
) -> Result<PathBuf> {
    fs::create_dir_all(directory)
        .with_context(|| format!("create repository directory {}", directory.display()))?;
    let directory = fs::canonicalize(directory)?;
    for suffix in 1_u64.. {
        let path = directory.join(if suffix == 1 {
            name.to_owned()
        } else {
            format!("{name}-{suffix}")
        });
        // A missing checkout still owns its recorded path.
        if repositories.iter().any(|repo| repo.path == path) {
            continue;
        }
        // Atomic reservation also prevents collisions between separate daemons.
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reserve clone directory {}", path.display()));
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
