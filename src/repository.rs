//! Repository naming and identity derived from a source path or URL.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{git, model::Repository};

use crate::forge::repo::remote_url::{RemoteUrl, source_name};

/// Preserve the caller's argument alongside a path resolved in its directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Selector {
    Value(String),
    Path {
        value: String,
        path: std::path::PathBuf,
    },
}

impl Selector {
    pub fn value(&self) -> &str {
        match self {
            Self::Value(value) | Self::Path { value, .. } => value,
        }
    }

    pub fn source(&self) -> Result<&str> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Path { path, .. } => path.to_str().context("repository path is not UTF-8"),
        }
    }

    fn path(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Value(value) if Path::new(value).is_absolute() => {
                std::fs::canonicalize(value).ok()
            }
            Self::Value(_) => None,
            Self::Path { path, .. } => Some(path.clone()),
        }
    }
}

impl From<String> for Selector {
    fn from(value: String) -> Self {
        Self::Value(value)
    }
}

impl From<&str> for Selector {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl From<&String> for Selector {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl From<&Selector> for Selector {
    fn from(value: &Selector) -> Self {
        value.clone()
    }
}

/// The explicit name, or the last path component of the source.
pub fn name(repo: &crate::model::Repository) -> &str {
    repo.name
        .as_deref()
        .unwrap_or_else(|| source_name(&repo.source))
}

/// A filesystem-safe clone directory name derived from the source.
pub fn directory_name(source: &str) -> String {
    let name: String = source_name(source)
        .chars()
        .take(crate::validate::MAX_NAME_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['.', '-', '_']);
    if name.is_empty() {
        "repository".into()
    } else {
        name.into()
    }
}

/// The host part of a remote URL (`https://user@host/...` or `user@host:...`),
/// without credentials.
pub fn host(source: &str) -> Option<&str> {
    RemoteUrl::parse(source).and_then(|remote| remote.host)
}

/// A remote URL with a host and repository path, as registration matches it.
pub fn is_remote_url(source: &str) -> bool {
    RemoteUrl::parse(source).is_some_and(|remote| remote.host.is_some() && remote.path.is_some())
}

/// Local checkouts use origin; clones retain their original source URL even
/// when their checkout is temporarily unavailable. No network access is needed.
pub async fn identity(source: &str) -> Result<Option<String>> {
    Ok(remote_url_from_source(source)
        .await?
        .map(|url| url_key(&url)))
}

/// Resolve a source that may be an existing checkout or a clone URL.
pub async fn remote_url_from_source(source: &str) -> Result<Option<String>> {
    let path = Path::new(source);
    if path.exists() {
        remote_url_from_path(path).await
    } else {
        Ok(Some(source.to_owned()))
    }
}

/// Read origin from a known checkout without interpreting its path as a URL.
pub async fn remote_url_from_path(path: &Path) -> Result<Option<String>> {
    let remotes = git::remotes(path, git::run).await?;
    if !remotes.lines().any(|remote| remote == "origin") {
        return Ok(None);
    }
    Ok(Some(
        git::remote_url(path, "origin").await?.trim().to_owned(),
    ))
}

/// What is wrong with a checkout whose Git config sets `core.bare` although it
/// has a working tree, and how to fix it; `None` when Git can use the working tree.
pub async fn bare_flag_problem(path: &Path) -> Result<Option<String>> {
    if !git::is_bare_with_work_tree(path).await? {
        return Ok(None);
    }
    let fix = crate::shell::quote(&[
        "git",
        "-C",
        &path.to_string_lossy(),
        "config",
        "core.bare",
        "false",
    ]);
    Ok(Some(format!(
        "{} has a working tree, but its Git config sets core.bare=true; run {fix}",
        path.display()
    )))
}

/// Normalize equivalent transports of one remote to a comparable key.
fn url_key(url: &str) -> String {
    RemoteUrl::parse(url)
        .map(|remote| remote.registration_key())
        .unwrap_or_else(|| url.to_owned())
}

/// Resolve the same repository selectors in the CLI and daemon.
pub async fn select(
    repositories: &[Repository],
    selector: impl Into<Selector>,
) -> Result<&Repository> {
    let selector = selector.into();
    let value = selector.value();
    if let Some(repo) = repositories
        .iter()
        .find(|repo| repo.id == value || repo.source == value)
    {
        return Ok(repo);
    }
    let canonical = selector.path();
    let exact = repositories
        .iter()
        .find(|repo| repo.name.as_deref() == Some(value));
    let mut matches: Vec<_> = repositories
        .iter()
        .filter(|repo| {
            exact.is_some_and(|exact| exact.id == repo.id)
                || (exact.is_none() && name(repo) == value)
                || repo.path.to_str() == Some(value)
                || canonical.as_ref() == Some(&repo.path)
        })
        .collect();
    ensure!(
        matches.len() <= 1,
        "repository name is ambiguous: {value}; matches {}; use its ID, path, or source URL",
        matches
            .iter()
            .map(|repo| format!("{} ({}, {})", name(repo), repo.id, repo.path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(repo) = matches.pop() {
        return Ok(repo);
    }
    if (canonical.is_some() || RemoteUrl::parse(value).is_some())
        && let Some(repo) = find_by_identity(repositories, selector.source()?)
            .await
            .with_context(|| match &canonical {
                Some(path) => format!(
                    "repository selector {value} was read as a path: {}",
                    path.display()
                ),
                None => format!("resolve repository selector: {value}"),
            })?
    {
        return Ok(repo);
    }
    Err(NotRegistered {
        value: value.to_owned(),
        path: canonical,
    }
    .into())
}

/// No registered repository matches the selector.
#[derive(Debug)]
pub struct NotRegistered {
    value: String,
    path: Option<std::path::PathBuf>,
}

impl std::fmt::Display for NotRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "repository is not registered: {}", self.value)?;
        if let Some(path) = &self.path {
            write!(f, " (read as a path: {})", path.display())?;
        }
        write!(f, "; run `shoal repo add <path-or-url>`")
    }
}

impl std::error::Error for NotRegistered {}

/// A local checkout or clone URL that registration can accept.
pub fn registrable(selector: &str) -> bool {
    Path::new(selector).exists() || RemoteUrl::parse(selector).is_some()
}

pub async fn find_by_identity<'a>(
    repositories: &'a [Repository],
    source: &str,
) -> Result<Option<&'a Repository>> {
    let Some(source_identity) = identity(source).await? else {
        return Ok(None);
    };
    for repo in repositories {
        if identity(&repo.source).await?.as_ref() == Some(&source_identity) {
            return Ok(Some(repo));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
