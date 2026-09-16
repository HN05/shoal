//! Repository registration and lookup; independent of worktree lifecycle.
use super::{Manager, validate_name};
use crate::{model::Repository, store, worktrunk};
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

    pub async fn register(
        &self,
        source: String,
        name: Option<String>,
        clone_path: Option<PathBuf>,
    ) -> Result<Repository> {
        if let Some(name) = &name {
            validate_name(name)?;
        }
        if let Some(path) = &clone_path {
            ensure!(path.is_absolute(), "repository clone path must be absolute");
            ensure!(
                !Path::new(&source).exists(),
                "--path is only for cloning URLs; local repositories are registered in place"
            );
        }
        let _guard = self.repositories.lock().await;
        let repositories = self.repositories().await?;
        if let Some(repo) = repositories.iter().find(|r| r.source == source) {
            check_clone_path(repo, clone_path.as_deref())?;
            return if let Some(name) = name {
                self.rename_repository(repo.id.clone(), name).await
            } else {
                Ok(repo.clone())
            };
        }
        if let Some(identity) = crate::repository::identity(&source).await? {
            for repo in &repositories {
                if crate::repository::identity(&repo.source).await?.as_ref() == Some(&identity) {
                    check_clone_path(repo, clone_path.as_deref())?;
                    return if let Some(name) = name {
                        self.rename_repository(repo.id.clone(), name).await
                    } else {
                        Ok(repo.clone())
                    };
                }
            }
        }
        let id = Uuid::new_v4().to_string();
        let path = if PathBuf::from(&source).exists() {
            let root =
                worktrunk::git(&PathBuf::from(&source), &["rev-parse", "--show-toplevel"]).await?;
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
            let path = match clone_path {
                Some(path) => path,
                None => self.config.repositories_dir(&self.paths)?.join(&id),
            };
            let directory = path
                .parent()
                .context("clone path must name a new directory")?;
            fs::create_dir_all(directory).with_context(|| {
                format!("create repository parent directory {}", directory.display())
            })?;
            // Only remove a directory on failure after this attempt created it.
            fs::create_dir(&path).with_context(|| {
                format!(
                    "clone destination must not already exist: {}",
                    path.display()
                )
            })?;
            let mut command = Command::new("git");
            command.args(["clone", "--"]).arg(&source).arg(&path);
            if let Err(error) = worktrunk::run(command).await {
                // This directory belongs exclusively to this attempt.
                if path.exists() {
                    fs::remove_dir_all(&path).context("clean up failed repository clone")?;
                }
                return Err(error);
            }
            fs::canonicalize(path)?
        };
        let path_string = path
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned();
        let repo = self.store.run(move |db| {
            db.execute("INSERT INTO repositories (id,path,source,last_used) VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(last_used), 0)+1 FROM repositories)) ON CONFLICT(path) DO NOTHING", params![id, path_string, source])?;
            Ok(db.query_row("SELECT * FROM repositories WHERE path=?1", [path_string], store::repository)?)
        }).await?;
        if let Some(name) = name {
            self.rename_repository(repo.id, name).await
        } else {
            Ok(repo)
        }
    }

    pub async fn rename_repository(&self, selector: String, name: String) -> Result<Repository> {
        validate_name(&name)?;
        let repo = self.repository(&selector).await?;
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

    pub(crate) async fn repository(&self, selector: &str) -> Result<Repository> {
        let repositories = self.repositories().await?;
        let canonical = fs::canonicalize(selector).ok();
        if let Some(repo) = repositories.iter().find(|repo| {
            repo.id == selector
                || repo.source == selector
                || repo.path.to_str() == Some(selector)
                || canonical.as_ref() == Some(&repo.path)
        }) {
            return Ok(repo.clone());
        }
        if let Some(repo) = repositories
            .iter()
            .find(|repo| repo.name.as_deref() == Some(selector))
        {
            return Ok(repo.clone());
        }
        if let Some(identity) = crate::repository::identity(selector).await? {
            for repo in repositories {
                if crate::repository::identity(&repo.source).await?.as_ref() == Some(&identity) {
                    return Ok(repo);
                }
            }
        }
        bail!("repository is not registered: {selector}; run `shoal repo add <path-or-url>`")
    }
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
